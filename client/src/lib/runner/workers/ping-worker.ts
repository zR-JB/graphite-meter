/* ============================================================
 * The Graphite Meter: latency ping worker
 * ============================================================
 * Owns the /ws/ping WebSocket and the ping algorithm. It runs off the main
 * thread so its in-worker timestamps keep RTT immune to main-thread jank,
 * which matters most for loaded latency, where the main thread is busiest.
 * Only computed { rtt, lost } samples cross the thread boundary.
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
import { sessionAuthenticationRequired } from "../../request-auth";

/** Main → worker. `start` opens + warms the bus (no reporting); `measure` flips
 *  reporting on for the SAME warmed socket; `stop` closes everything. */
type InMsg =
  | {
      type: "start";
      url: string;
      intervalMs: number;
      replyDriven: boolean;
      maxInFlight: number;
      reportGapMs: number;
      lossK: number;
      lossFloorMs: number;
      checkAuthentication?: boolean;
    }
  | { type: "measure"; intervalMs?: number }
  | { type: "stop" };

/** Worker → main. Samples downsample to reportGapMs, so a ~1 kHz chain cannot
 *  flood host.ingestLatency, then batch every ~50 ms to cut postMessage
 *  overhead. Both affect only how many samples cross the boundary: the rtt is
 *  timestamped in-worker. stall/resume bracket a reconnect window. */
type OutMsg =
  | { type: "open" }
  | { type: "ready" }
  | { type: "samples"; samples: { rtt: number; lost: boolean }[] }
  | { type: "stall"; detail: string }
  | { type: "resume" }
  | { type: "auth-required" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg): void => ctx.postMessage(m);

/** Batch flush cadence (ms). */
const FLUSH_MS = 50;
/** Upper bound on the loss timeout (ms). Caps how long a ping stays pending on
 *  a pathologically slow link, keeping memory and latency bounded. */
const LOSS_CEIL_MS = 10_000;
/** Reconnect backoff bounds (ms). */
const RECONNECT_MIN_MS = 100;
const RECONNECT_MAX_MS = 2_000;
/** Recently-evicted ids kept for late-pong learning (bounded, FIFO). */
const GRAVEYARD_MAX = 64;
const REPLY_BACKUP_INITIAL_MS = 250;
const REPLY_BACKUP_FLOOR_MS = 8;
const REPLY_BACKUP_CEIL_MS = 1_000;

let url = "";
let ws: WebSocket | null = null;
let measuring = false;
let stopped = false;
let checkAuthentication = false;

// Tuning: stage workers keep this fixed; the idle worker settles from probe
// pacing to its one-second keepalive interval.
let intervalMs = 250;
let replyDriven = false;
let maxInFlight = 16; // caps concurrent pings, bounding wire spam and memory
let reportGapMs = 20;
let lossK = 4;
let lossFloorMs = 250;

// Send/pending state.
const pending = new Map<number, number>(); // id → sendTime (performance.now())
const graveyard = new Map<number, number>(); // evicted id → sendTime (late-pong learning)
let nextId = 0; // client-owned monotonic uint32
let replyHeadId: number | null = null;
let lastReportAt = 0; // gates the UI-bound sample rate (see reportGapMs)
let outbox: { rtt: number; lost: boolean }[] = [];

// Adaptive RTT estimator (RFC 6298, ms). See rttEstimator.ts.
let rttEstimate: RttEstimate = INITIAL_RTT_ESTIMATE;

// Connection state.
let backoff = 0;
let stalledOut = false; // true between a `stall` and its matching `resume`
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

let scheduler: PingScheduler | null = null;

// Long-lived timers, one each, surviving reconnects.
let sweeper: ReturnType<typeof setInterval> | null = null;
let flusher: ReturnType<typeof setInterval> | null = null;

ctx.onmessage = (e: MessageEvent<InMsg>): void => {
  const m = e.data;
  switch (m.type) {
    case "start":
      url = m.url;
      intervalMs = m.intervalMs;
      replyDriven = m.replyDriven;
      maxInFlight = m.maxInFlight;
      reportGapMs = m.reportGapMs;
      lossK = m.lossK;
      lossFloorMs = m.lossFloorMs;
      checkAuthentication = m.checkAuthentication ?? false;
      scheduler = new PingScheduler(
        replyDriven
          ? { kind: "reply-driven", backupDelayMs: replyBackupDelay }
          : { kind: "fixed", intervalMs },
        (now) => {
          if (!ws || ws.readyState !== WebSocket.OPEN) return false;
          if (pending.size >= maxInFlight) return false;
          sendPing(now);
          return true;
        },
      );
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
    // Warmup hello: the server replies READY. Pinging starts immediately so the
    // wire is already warm when `measure` flips reporting on.
    trySend(encode({ op: "HI", proto: "ws" }));
    scheduler?.reset();
    scheduler?.start();
  };
  ws.onmessage = (ev: MessageEvent): void => onFrame(ev.data);
  // A WebSocket always follows onerror with onclose. Reconnect from onclose
  // only, to avoid a double schedule.
  ws.onclose = (event: CloseEvent): void => {
    if (event.code === 1008 && event.reason === "authentication required") {
      post({ type: "auth-required" });
      stopped = true;
      return;
    }
    if (checkAuthentication && event.code === 1006) {
      void checkSessionThenReconnect();
      return;
    }
    onDisconnect("websocket closed");
  };
}

async function checkSessionThenReconnect(): Promise<void> {
  if (await sessionAuthenticationRequired(self.location.origin)) {
    post({ type: "auth-required" });
    stopped = true;
    return;
  }
  onDisconnect("websocket closed");
}

function onDisconnect(detail: string): void {
  if (stopped) return;
  ws = null;
  scheduler?.stop();
  // In-flight pings die with the socket. Dropping them silently is correct: a
  // connection gap is not per-packet loss, and the `stall` reports the gap.
  pending.clear();
  scheduleReconnect(detail);
}

/** Brackets the connection gap: `stall` on the way out, `resume` on reopen,
 *  with capped backoff between attempts, so a dropped socket self-heals. */
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
  flusher ??= setInterval(flush, FLUSH_MS);
}

function onFrame(data: unknown): void {
  const recv = performance.now();
  if (typeof data !== "string") return; // the ping bus is text-only
  let frame;
  try {
    frame = decode(data);
  } catch {
    return; // malformed and ERR frames never tear the bus down
  }
  if (frame.op === "READY") {
    post({ type: "ready" });
    return;
  }
  if (frame.op !== "PONG") return;

  const sent = pending.get(frame.id);
  if (sent !== undefined) {
    pending.delete(frame.id);
    const rtt = recv - sent;
    rttEstimate = observeRtt(rttEstimate, rtt); // always: keeps the loss timeout accurate
    // Reply-driven localhost sampling can outrun the UI. Only downsample what
    // crosses the worker boundary; wire pacing and RTT timestamps stay intact.
    if (measuring && recv - lastReportAt >= reportGapMs) {
      lastReportAt = recv;
      outbox.push({ rtt, lost: false });
    }
    if (!replyDriven || frame.id === replyHeadId) scheduler?.complete();
    return;
  }

  // A pong for an evicted ping still teaches the estimator, which is the only
  // way it learns a fast to slow jump: lost pings carry no RTT of their own.
  const late = graveyard.get(frame.id);
  if (late !== undefined) {
    graveyard.delete(frame.id);
    rttEstimate = observeRtt(rttEstimate, recv - late); // already counted lost, so no sample
  }
  // Unknown and duplicate ids fall through, ignored.
}

/** Sends one PING under a client-owned id. Several stay in flight on a high-RTT
 *  link, and the id is what matches an out-of-order pong to its send time. */
function sendPing(now: number): void {
  const id = nextId;
  // The in-flight window is tiny next to 2^32, so a wrapped id cannot collide
  // with a still-pending one.
  nextId = (nextId + 1) >>> 0;
  pending.set(id, now);
  if (replyDriven) replyHeadId = id;
  trySend(encode({ op: "PING", id }));
}

function replyBackupDelay(): number {
  if (!rttEstimate.haveRtt) return REPLY_BACKUP_INITIAL_MS;
  return lossTimeout(
    rttEstimate,
    lossK,
    REPLY_BACKUP_FLOOR_MS,
    REPLY_BACKUP_CEIL_MS,
  );
}

function trySend(msg: string): void {
  try {
    ws?.send(msg);
  } catch {
    /* closed mid-send: onclose drives the reconnect */
  }
}

/** Evicts pings past the adaptive timeout and counts them lost. Over TCP/WS that
 *  means a stalled socket or queue, not packet loss: TCP retransmits. Real loss
 *  needs WebTransport datagrams (docs/ARCHITECTURE.md#roadmap). */
function sweep(): void {
  const now = performance.now();
  const timeout = lossTimeout(rttEstimate, lossK, lossFloorMs, LOSS_CEIL_MS);
  let evicted = false;
  let replyHeadEvicted = false;
  for (const [id, sent] of pending) {
    if (now - sent > timeout) {
      pending.delete(id);
      rememberEvicted(id, sent);
      evicted = true;
      if (id === replyHeadId) replyHeadEvicted = true;
      if (measuring) outbox.push({ rtt: now - sent, lost: true });
    }
  }
  // A timed-out request completes one chain step. Fixed pacing still respects
  // its boundary; reply-driven pacing replaces it immediately.
  if (evicted && (!replyDriven || replyHeadEvicted)) scheduler?.complete();
}

/** Stashes an evicted id so a late pong can still teach the estimator. The FIFO
 *  is bounded: genuinely lost ids age out. */
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
  replyHeadId = null;
  try {
    ws?.close(1000, "");
  } catch {
    /* already closed */
  }
  ws = null;
}
