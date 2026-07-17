/* ============================================================
 * The Graphite Meter — Latency ping worker (Stage 4)
 * ============================================================
 *
 * Owns the WebSocket latency bus (/ws/ping) and the entire ping algorithm. It
 * runs OFF the main thread on purpose: it timestamps each ping in-worker
 * (performance.now() right before send / right after receive) so the reported
 * RTT is immune to main-thread jank — GC, the 16 Hz throughput aggregation, UI
 * layout. That matters most for LOADED latency, where the main thread is busy
 * with exactly the transfer we're measuring against. The worker reports only
 * computed { rtt, lost } samples; raw frames never cross the thread boundary.
 *
 * ── Cadenced send (accurate from sub-1 ms to seconds of RTT) ──
 *   • A start-to-start scheduler enforces the configured minimum interval. A
 *     PONG can free capacity and resume an overdue sender, but never starts an
 *     early PING or a catch-up burst.
 *   • Multiple pings may remain in flight on high-RTT links. Each carries its
 *     own id, so out-of-order pongs match.
 *   • maxInFlight cap: bounds wire spam AND memory.
 *
 * ── Robustness to abrupt change (AP / WiFi→mobile handoff) ──
 *   • Adaptive loss timeout: RFC 6298-style RTO = SRTT + K·RTTVAR (in ms). The
 *     RTTVAR term spikes on a sudden RTT jump, so the timeout grows within ~1 RTT
 *     instead of mass-evicting legitimately-in-flight pings.
 *   • Late-pong learning (the key to surviving a fast→slow jump): a pong that
 *     arrives AFTER we already declared its ping lost is matched against a small
 *     graveyard of recently-evicted ids; we feed its RTT into the estimator (so
 *     the timeout catches up) without double-counting it. Without this, the
 *     estimator could never learn the link slowed — lost pings carry no RTT — and
 *     would false-flag loss forever.
 *   • Loss = timeout-only: over TCP/WS this is a stalled socket/queue, NOT real
 *     packet loss (TCP retransmits). Real measurable loss is WT datagrams (Stage 5).
 *   • Auto-reconnect: a handoff often drops the TCP socket outright. On an
 *     unexpected close the worker clears in-flight pings (a connection gap, not
 *     per-packet loss), emits `stall`, and reconnects with capped backoff; on
 *     reopen it re-warms (HI) and emits `resume`. The run self-heals.
 *
 * Its only dependency is the shared wire codec, so it bundles cleanly as a Vite
 * module worker.
 * ============================================================ */

import { encode, decode } from "../real/wire";
import {
  observeRtt,
  lossTimeout,
  INITIAL_RTT_ESTIMATE,
  type RttEstimate,
} from "./rttEstimator";
import { nextBackoff } from "./backoff";
import { PingScheduler } from "./pingScheduler";

/** Main → worker. `start` opens + warms the bus (no reporting); `measure` flips
 *  reporting on for the SAME warmed socket; `stop` closes everything. */
type InMsg =
  | {
      type: "start";
      url: string;
      intervalMs: number;
      maxInFlight: number;
      reportGapMs: number;
      lossK: number;
      lossFloorMs: number;
    }
  | { type: "measure"; intervalMs?: number }
  | { type: "stop" };

/** Worker → main. Samples are DOWNSAMPLED to reportGapMs (so a ~1 kHz chain on a
 *  fast link doesn't flood host.ingestLatency) and then batched (~50 ms) to cut
 *  postMessage overhead. The rtt is timestamped in-worker, so neither downsample
 *  nor batch affects the measured value — only how many cross the thread boundary.
 *  stall/resume bracket a reconnect window. */
type OutMsg =
  | { type: "open" }
  | { type: "samples"; samples: { rtt: number; lost: boolean }[] }
  | { type: "stall"; detail: string }
  | { type: "resume" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg): void => ctx.postMessage(m);

/** Batch flush cadence (ms). */
const FLUSH_MS = 50;
/** Upper bound on the loss timeout (ms) — caps how long a ping can stay pending
 *  even on a pathologically slow link, so memory/latency stay bounded. */
const LOSS_CEIL_MS = 10_000;
/** Reconnect backoff bounds (ms). */
const RECONNECT_MIN_MS = 100;
const RECONNECT_MAX_MS = 2_000;
/** Recently-evicted ids kept for late-pong learning (bounded, FIFO). */
const GRAVEYARD_MAX = 64;

let url = "";
let ws: WebSocket | null = null;
let measuring = false;
let stopped = false;

// Tuning — fixed for the lifetime of the stage-owned worker.
let intervalMs = 250;
let maxInFlight = 16;
let reportGapMs = 20;
let lossK = 4;
let lossFloorMs = 250;

// Send/pending state.
const pending = new Map<number, number>(); // id → sendTime (performance.now())
const graveyard = new Map<number, number>(); // evicted id → sendTime (late-pong learning)
let nextId = 0; // client-owned monotonic uint32
let lastReportAt = 0; // gates the UI-bound sample rate (see reportGapMs)
let outbox: { rtt: number; lost: boolean }[] = [];

// Adaptive RTT estimator (RFC 6298, ms) — see rttEstimator.ts.
let rttEstimate: RttEstimate = INITIAL_RTT_ESTIMATE;

// Connection state.
let backoff = 0;
let stalledOut = false; // a `stall` emitted, not yet matched by `resume`
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

let scheduler: PingScheduler | null = null;

// Long-lived timers (started once, survive reconnects).
let sweeper: ReturnType<typeof setInterval> | null = null;
let flusher: ReturnType<typeof setInterval> | null = null;

ctx.onmessage = (e: MessageEvent<InMsg>): void => {
  const m = e.data;
  switch (m.type) {
    case "start":
      url = m.url;
      intervalMs = m.intervalMs;
      maxInFlight = m.maxInFlight;
      reportGapMs = m.reportGapMs;
      lossK = m.lossK;
      lossFloorMs = m.lossFloorMs;
      scheduler = new PingScheduler(intervalMs, (now) => {
        if (!ws || ws.readyState !== WebSocket.OPEN) return false;
        if (pending.size >= maxInFlight) return false;
        sendPing(now);
        return true;
      });
      ensureTimers();
      connect();
      break;
    case "measure":
      if (m.intervalMs !== undefined) {
        intervalMs = m.intervalMs;
        scheduler?.setInterval(intervalMs);
      }
      measuring = true;
      lastReportAt = 0; // report the first measured sample promptly
      break;
    case "stop":
      teardown();
      break;
  }
};

function connect(): void {
  if (stopped) return;
  try {
    ws = new WebSocket(url);
  } catch (err) {
    scheduleReconnect(String(err));
    return;
  }
  ws.onopen = (): void => {
    backoff = 0;
    if (stalledOut) {
      post({ type: "resume" });
      stalledOut = false;
    }
    post({ type: "open" });
    // Optional warmup hello — the server replies READY (ignored). Then start the
    // chain immediately so the wire is warm before `measure` flips reporting on.
    trySend(encode({ op: "HI", proto: "ws" }));
    scheduler?.reset();
    scheduler?.start();
  };
  ws.onmessage = (ev: MessageEvent): void => onFrame(ev.data);
  // onerror is always followed by onclose for WebSocket — handle reconnect once,
  // in onclose, to avoid a double schedule.
  ws.onclose = (): void => onDisconnect("websocket closed");
}

function onDisconnect(detail: string): void {
  if (stopped) return;
  ws = null;
  scheduler?.stop();
  // The socket is gone — its in-flight pings died with it. Drop them silently:
  // a connection gap is not per-packet loss (the `stall` represents the gap).
  pending.clear();
  scheduleReconnect(detail);
}

function scheduleReconnect(detail: string): void {
  if (stopped) return;
  if (!stalledOut) {
    post({ type: "stall", detail });
    stalledOut = true;
  }
  backoff = nextBackoff(backoff, RECONNECT_MIN_MS, RECONNECT_MAX_MS);
  reconnectTimer = setTimeout(connect, backoff);
}

function ensureTimers(): void {
  // Eviction sweep: drop pings stalled past the adaptive timeout.
  sweeper ??= setInterval(sweep, Math.max(lossFloorMs, intervalMs));
  // Batch flush.
  flusher ??= setInterval(flush, FLUSH_MS);
}

function onFrame(data: unknown): void {
  const recv = performance.now();
  if (typeof data !== "string") return; // the ping bus is text-only
  let f;
  try {
    f = decode(data);
  } catch {
    return; // ignore malformed / ERR frames — never tear the bus down
  }
  if (f.op !== "PONG") return; // READY (warmup ack) and anything else: ignore

  const sent = pending.get(f.id);
  if (sent !== undefined) {
    pending.delete(f.id);
    const rtt = recv - sent;
    rttEstimate = observeRtt(rttEstimate, rtt); // ALWAYS — keeps the loss-timeout estimator accurate
    // Downsample the UI-bound stream: on a fast link the on-receive chain fires
    // far faster than any chart needs (~1 kHz on localhost), so report at most
    // one sample per reportGapMs. The pings stay fast (responsiveness + a full
    // in-flight window); only what crosses to the main thread is capped, so
    // host.ingestLatency isn't called thousands of times/sec.
    if (measuring && recv - lastReportAt >= reportGapMs) {
      lastReportAt = recv;
      outbox.push({ rtt, lost: false });
    }
    scheduler?.nudge();
    return;
  }

  // Late pong: we already declared this ping lost (timeout too tight — typically
  // an abrupt RTT jump). LEARN from it so the timeout grows and we stop
  // false-flagging; don't emit (already counted lost).
  const late = graveyard.get(f.id);
  if (late !== undefined) {
    graveyard.delete(f.id);
    rttEstimate = observeRtt(rttEstimate, recv - late);
    scheduler?.nudge();
  }
  // else: unknown / duplicate id — ignore.
}

function sendPing(now: number): void {
  const id = nextId;
  nextId = (nextId + 1) >>> 0; // uint32 wrap — the in-flight window is tiny, so a
  // wrapped id can never collide with a still-pending one.
  pending.set(id, now);
  trySend(encode({ op: "PING", id }));
}

function trySend(msg: string): void {
  try {
    ws?.send(msg);
  } catch {
    /* closed mid-send — onclose drives the reconnect */
  }
}

function sweep(): void {
  const now = performance.now();
  const timeout = lossTimeout(rttEstimate, lossK, lossFloorMs, LOSS_CEIL_MS);
  let evicted = false;
  for (const [id, sent] of pending) {
    if (now - sent > timeout) {
      pending.delete(id);
      rememberEvicted(id, sent);
      evicted = true;
      if (measuring) outbox.push({ rtt: now - sent, lost: true });
    }
  }
  // Nudge ONLY when an eviction freed a slot the cap was blocking on. An
  // unconditional nudge would add a send per sweep tick on top of the pacer,
  // breaking the paced (chain-off) modes' send rate.
  if (evicted) scheduler?.nudge();
}

/** Stash an evicted id so a late pong can still teach the estimator. Bounded
 *  FIFO — genuinely-lost ids just age out. */
function rememberEvicted(id: number, sent: number): void {
  graveyard.set(id, sent);
  if (graveyard.size > GRAVEYARD_MAX) {
    const oldest = graveyard.keys().next().value;
    if (oldest !== undefined) graveyard.delete(oldest);
  }
}

function flush(): void {
  if (outbox.length === 0) return;
  const samples = outbox;
  outbox = [];
  post({ type: "samples", samples });
}

function teardown(): void {
  stopped = true;
  measuring = false;
  if (reconnectTimer !== null) clearTimeout(reconnectTimer);
  reconnectTimer = null;
  scheduler?.stop();
  scheduler = null;
  for (const t of [sweeper, flusher]) if (t !== null) clearInterval(t);
  sweeper = flusher = null;
  flush(); // emit any tail
  pending.clear();
  graveyard.clear();
  try {
    ws?.close(1000, "");
  } catch {
    /* already closed */
  }
  ws = null;
}
