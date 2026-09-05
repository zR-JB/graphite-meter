/* Every measured outcome crosses the boundary with its worker-owned observation time. */

import { encode, decode } from "../real/wire";
import {
  observeRtt,
  lossTimeout,
  INITIAL_RTT_ESTIMATE,
  type RttEstimate,
} from "./rttEstimator";
import { nextBackoff } from "./backoff";
import { mintWtToken, spendWtToken, withWtToken, type WtMint } from "./wtToken";
import { createPingScheduler, type PingScheduler } from "./pingScheduler";
import { sessionAuthenticationRequired } from "../../request-auth";
import { ESTABLISH_BUDGET_MS } from "../real/budgets";
import {
  pingSample,
  reflectorHandlingMs,
  PING_TIMEOUT_CEIL_MS,
  type PingSample,
  type PingWorkerEvent,
  type PingInterruptionReason,
} from "./pingSample";

/* Main → worker. */
type InMsg =
  | {
      type: "start";
      url: string;
      transport: "websocket" | "webtransport";
      mint?: WtMint;
      intervalMs: number;
      replyDriven: boolean;
      maxInFlight: number;
      lossK: number;
      lossFloorMs: number;
      checkAuthentication?: boolean;
    }
  | { type: "measure"; intervalMs?: number }
  | { type: "stop"; cutoffEpochMs: number };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: PingWorkerEvent): void => ctx.postMessage(m);

/** Batch flush cadence (ms). */
const FLUSH_MS = 50;
const MAX_BATCH_SAMPLES = 128;
/** Reconnect backoff bounds (ms). */
const RECONNECT_MIN_MS = 100;
const RECONNECT_MAX_MS = 2_000;
/** Recently-evicted ids kept for late-pong learning (bounded, FIFO). */
const GRAVEYARD_MAX = 64;
const REPLY_BACKUP_INITIAL_MS = 250;
const REPLY_BACKUP_FLOOR_MS = 8;
const REPLY_BACKUP_CEIL_MS = 1_000;

interface PingLink {
  ready(): boolean;
  send(msg: string): void | Promise<void>;
  close(): void;
}

let url = "";
let transport: "websocket" | "webtransport" = "websocket";
let mint: WtMint | undefined;
let link: PingLink | null = null;
let measuring = false;
let stopped = false;
let stopCutoff: number | null = null;
let checkAuthentication = false;
let timingNegotiated = false;

// Tuning: stage workers keep this fixed; the idle worker settles from probe pacing to its one-second keepalive.
let intervalMs = 250;
let replyDriven = false;
let maxInFlight = 16; // caps concurrent pings, bounding wire spam and memory
let lossK = 4;
let lossFloorMs = 250;

interface PendingPing {
  sentAt: number;
  expiresAt: number;
  writeConfirmed: boolean;
  /* Warmup replies never enter the measured population. */
  measured: boolean;
}

// Send/pending state.
const pending = new Map<number, PendingPing>();
const graveyard = new Map<number, number>(); // evicted id → sendTime (late-pong learning)
let nextId = 0; // client-owned monotonic uint32
let replyHeadId: number | null = null;
let outbox: PingSample[] = [];

// Adaptive RTT estimator (RFC 6298, ms). See rttEstimator.ts.
let rttEstimate: RttEstimate = INITIAL_RTT_ESTIMATE;

// Connection state.
let backoff = 0;
let stalledOut = false; // true between a `stall` and its matching `resume`

let scheduler: PingScheduler | null = null;

// Long-lived timers, one each, surviving reconnects.
let sweeper: ReturnType<typeof setInterval> | null = null;
let flusher: ReturnType<typeof setInterval> | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let drainTimer: ReturnType<typeof setTimeout> | null = null;

ctx.onmessage = (e: MessageEvent<InMsg>): void => {
  const m = e.data;
  switch (m.type) {
    case "start":
      url = m.url;
      transport = m.transport;
      mint = m.mint;
      intervalMs = m.intervalMs;
      replyDriven = m.replyDriven;
      maxInFlight = m.maxInFlight;
      lossK = m.lossK;
      lossFloorMs = m.lossFloorMs;
      checkAuthentication = m.checkAuthentication ?? false;
      scheduler = createPingScheduler(
        replyDriven
          ? { kind: "reply-driven", backupDelayMs: replyBackupDelay }
          : { kind: "fixed", intervalMs },
        (now) => {
          if (!link?.ready()) return false;
          if (pending.size >= maxInFlight) return false;
          sendPing(now);
          return true;
        },
      );
      ensureTimers();
      connect();
      break;
    case "measure":
      if (stopped || stopCutoff !== null) return;
      if (m.intervalMs !== undefined) {
        intervalMs = m.intervalMs;
        scheduler?.setInterval(intervalMs);
      }
      measuring = true;
      if (!replyDriven) scheduler?.restartNow();
      break;
    case "stop":
      beginStop(m.cutoffEpochMs);
      break;
  }
};

function connect(): void {
  if (stopped || stopCutoff !== null) return;
  if (transport === "webtransport") void connectWebTransport();
  else connectWebSocket();
}

/* Announces an open bus and starts the chain. */
function onConnected(proto: string): void {
  if (stopped || stopCutoff !== null) return;
  backoff = 0;
  if (stalledOut) {
    post({ type: "resume" });
    stalledOut = false;
  }
  post({ type: "open" });
  timingNegotiated = false;
  trySend(encode({ op: "HI", proto, timing: true }));
  scheduler?.reset();
  scheduler?.start();
}

function connectWebSocket(): void {
  let ws: WebSocket;
  try {
    ws = new WebSocket(url);
  } catch (err) {
    scheduleReconnect(String(err));
    return;
  }
  const connection: PingLink = {
    ready: () => ws.readyState === WebSocket.OPEN,
    send: (msg) => ws.send(msg),
    close: () => ws.close(),
  };
  link = connection;
  ws.onopen = (): void => {
    if (link === connection) onConnected("ws");
  };
  ws.onmessage = (ev: MessageEvent): void => {
    if (link === connection && connection.ready()) onFrame(ev.data);
  };
  // A WebSocket always follows onerror with onclose. Reconnect from onclose only, to avoid a double schedule.
  ws.onclose = (event: CloseEvent): void => {
    if (stopped || link !== connection) return;
    if (event.code === 1008 && event.reason === "authentication required") {
      interruptPending("unresolved");
      post({ type: "auth-required" });
      finishStop();
      return;
    }
    if (stopCutoff !== null) {
      onDisconnect("websocket closed");
      return;
    }
    if (checkAuthentication && event.code === 1006) {
      void checkSessionThenReconnect("websocket closed");
      return;
    }
    onDisconnect("websocket closed");
  };
}

/** One wire message per datagram, so the read loop needs no framing. */
async function connectWebTransport(): Promise<void> {
  const minted = await mintWtToken(mint);
  if (stopped || stopCutoff !== null) return;
  // An authenticated bus cannot dial without a token.
  if (checkAuthentication && mint && minted.token === "") {
    if (minted.authRequired) {
      post({ type: "auth-required" });
      finishStop();
      return;
    }
    scheduleReconnect("webtransport token mint failed");
    return;
  }
  const token = minted.token;
  let wt: WebTransport;
  try {
    wt = new WebTransport(withWtToken(url, token), {
      congestionControl: "low-latency",
    });
  } catch (err) {
    scheduleReconnect(String(err));
    return;
  }
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  let writer: WritableStreamDefaultWriter<Uint8Array> | null = null;
  let disconnectReported = false;
  const disconnect = (detail: string): void => {
    if (disconnectReported || link !== connection) return;
    disconnectReported = true;
    onDisconnect(detail);
  };
  const connection: PingLink = {
    ready: () => writer !== null,
    send: (msg) => {
      if (!writer) throw new Error("datagram writer unavailable");
      return writer.write(encoder.encode(msg));
    },
    close: () => wt.close(),
  };
  link = connection;
  void wt.closed.then(
    () => disconnect("webtransport closed"),
    (err: unknown) => disconnect(String(err)),
  );
  let timer: ReturnType<typeof setTimeout> | null = null;
  try {
    await Promise.race([
      wt.ready,
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("webtransport session did not establish")),
          ESTABLISH_BUDGET_MS,
        );
      }),
    ]);
  } catch (err) {
    if (timer !== null) clearTimeout(timer);
    try {
      wt.close();
    } catch {
      /* already closing */
    }
    // Some implementations leave `closed` pending with a black-holed handshake, so the deadline itself must drive the.
    disconnect(String(err));
    return;
  }
  if (timer !== null) clearTimeout(timer);
  if (stopped || stopCutoff !== null || link !== connection) {
    wt.close();
    return;
  }
  // `ready` fulfils on the CONNECT the server accepted, which is the moment it deleted the token.
  spendWtToken(token);
  try {
    writer = wt.datagrams.writable.getWriter();
    onConnected("wt");
    const reader = wt.datagrams.readable.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) {
        disconnect("webtransport datagram stream closed");
        return;
      }
      if (link !== connection) return;
      onFrame(decoder.decode(value as AllowSharedBufferSource));
    }
  } catch (err) {
    try {
      wt.close();
    } catch {
      /* already closing */
    }
    disconnect(String(err));
  }
}

async function checkSessionThenReconnect(detail: string): Promise<void> {
  if (await sessionAuthenticationRequired(self.location.origin)) {
    if (stopped) return;
    interruptPending("unresolved");
    post({ type: "auth-required" });
    finishStop();
    return;
  }
  onDisconnect(detail);
}

function onDisconnect(detail: string): void {
  if (stopped) return;
  link = null;
  timingNegotiated = false;
  scheduler?.stop();
  sweep();
  if (stopped) return;
  interruptPending("unresolved");
  if (stopCutoff !== null) {
    finishStop();
    return;
  }
  scheduleReconnect(detail);
}

/* Brackets the connection gap: `stall` on the way out, `resume` on reopen, with capped backoff between attempts. */
function scheduleReconnect(detail: string): void {
  if (stopped || stopCutoff !== null) return;
  if (!stalledOut) {
    post({ type: "stall", detail });
    stalledOut = true;
  }
  backoff = nextBackoff(backoff, RECONNECT_MIN_MS, RECONNECT_MAX_MS);
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, backoff);
}

function ensureTimers(): void {
  // Eviction sweep: drop pings stalled past the adaptive timeout.
  sweeper ??= setInterval(sweep, Math.max(lossFloorMs, intervalMs));
  flusher ??= setInterval(flush, FLUSH_MS);
}

function onFrame(data: unknown): void {
  if (stopped) return;
  const recv = performance.now();
  if (typeof data !== "string") return; // the ping bus is text-only
  let frame;
  try {
    frame = decode(data);
  } catch {
    return; // malformed and ERR frames never tear the bus down
  }
  if (frame.op === "READY") {
    timingNegotiated ||= frame.timing === true;
    post({ type: "ready" });
    return;
  }
  if (frame.op !== "PONG") return;

  const ping = pending.get(frame.id);
  if (ping !== undefined) {
    pending.delete(frame.id);
    const rtt = recv - ping.sentAt;
    rttEstimate = observeRtt(rttEstimate, rtt); // always: keeps the loss timeout accurate; reply-driven localhost.
    if (eligible(ping))
      recordOutcome(
        ping,
        recv >= ping.expiresAt,
        recv >= ping.expiresAt ? ping.expiresAt : recv,
        timingNegotiated ? frame.handlingNanos : undefined,
      );
    if (!replyDriven || frame.id === replyHeadId) scheduler?.complete();
    serviceDrain();
    return;
  }

  // A pong for an evicted ping still teaches the estimator, which is the only way it learns a fast to slow jump: lost.
  const late = graveyard.get(frame.id);
  if (late !== undefined) {
    graveyard.delete(frame.id);
    rttEstimate = observeRtt(rttEstimate, recv - late); // already counted lost, so no sample
  }
  // Unknown and duplicate ids fall through, ignored.
}

/* Several stay in flight on a high-RTT link, and the id is what matches an out-of-order pong to its send time. */
function sendPing(now: number): void {
  const id = nextId;
  // The in-flight window is tiny next to 2^32, so a wrapped id cannot collide with a still-pending one.
  nextId = (nextId + 1) >>> 0;
  if (replyDriven) replyHeadId = id;
  const ping: PendingPing = {
    sentAt: now,
    expiresAt:
      now + lossTimeout(rttEstimate, lossK, lossFloorMs, PING_TIMEOUT_CEIL_MS),
    writeConfirmed: false,
    measured: measuring,
  };
  pending.set(id, ping);
  const failed = (): void => {
    if (pending.get(id) !== ping) return;
    pending.delete(id);
    if (eligible(ping)) {
      // The interruption must follow any replies already observed in this batch.
      flush();
      post({
        type: "interrupted",
        sentAtEpochMs: [performance.timeOrigin + ping.sentAt],
        reason: "send-failed",
      });
    }
    serviceDrain();
  };
  try {
    const sent = link!.send(encode({ op: "PING", id }));
    if (sent) {
      void sent.then(() => {
        ping.writeConfirmed = true;
        serviceDrain();
      }, failed);
    } else ping.writeConfirmed = true;
  } catch {
    failed();
  }
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
    void link?.send(msg)?.catch(() => {});
  } catch {
    /* closed mid-send: the close handler drives the reconnect */
  }
}

/* Resolve each probe against the deadline fixed when it was submitted. */
function sweep(): void {
  if (stopped) return;
  const now = performance.now();
  let evicted = false;
  let replyHeadEvicted = false;
  for (const [id, ping] of pending) {
    if (ping.writeConfirmed && now >= ping.expiresAt) {
      pending.delete(id);
      rememberEvicted(id, ping.sentAt);
      evicted = true;
      if (id === replyHeadId) replyHeadEvicted = true;
      if (eligible(ping)) recordOutcome(ping, true, ping.expiresAt);
    }
  }
  // A timed-out request completes one chain step.
  if (evicted && (!replyDriven || replyHeadEvicted)) scheduler?.complete();
  serviceDrain();
}

function eligible(ping: PendingPing): boolean {
  return ping.measured && (stopCutoff === null || ping.sentAt <= stopCutoff);
}

function recordOutcome(
  ping: PendingPing,
  lost: boolean,
  observedAt: number,
  handlingNanos?: string,
): void {
  const handling = lost
    ? undefined
    : reflectorHandlingMs(observedAt - ping.sentAt, handlingNanos);
  record({
    ...pingSample(observedAt - ping.sentAt, lost, observedAt),
    sentAtEpochMs: performance.timeOrigin + ping.sentAt,
    ...(handling === undefined ? {} : { reflectorHandlingMs: handling }),
  });
}

function interruptPending(reason: PingInterruptionReason): void {
  const sentAtEpochMs = [...pending.values()]
    .filter(eligible)
    .map((ping) => performance.timeOrigin + ping.sentAt);
  pending.clear();
  flush();
  if (sentAtEpochMs.length)
    post({ type: "interrupted", sentAtEpochMs, reason });
}

function beginStop(cutoffEpochMs: number): void {
  if (stopped) {
    post({ type: "stopped" });
    return;
  }
  if (stopCutoff !== null) return;
  stopCutoff = cutoffEpochMs - performance.timeOrigin;
  scheduler?.stop();
  if (reconnectTimer !== null) clearTimeout(reconnectTimer);
  reconnectTimer = null;
  for (const [id, ping] of pending) if (!eligible(ping)) pending.delete(id);
  outbox = outbox.filter(
    (sample) => (sample.sentAtEpochMs ?? 0) <= cutoffEpochMs,
  );
  flush();
  serviceDrain();
}

function serviceDrain(): void {
  if (stopped || stopCutoff === null) return;
  if (drainTimer !== null) clearTimeout(drainTimer);
  drainTimer = null;
  if (!pending.size) {
    finishStop();
    return;
  }
  const now = performance.now();
  const deadline = Math.min(
    stopCutoff + PING_TIMEOUT_CEIL_MS,
    Math.max(...[...pending.values()].map((ping) => ping.expiresAt)),
  );
  if (now >= deadline) {
    for (const [id, ping] of pending) {
      if (!ping.writeConfirmed) continue;
      pending.delete(id);
      if (eligible(ping)) recordOutcome(ping, true, ping.expiresAt);
    }
    interruptPending("unresolved");
    finishStop();
    return;
  }
  drainTimer = setTimeout(serviceDrain, deadline - now);
}

function finishStop(): void {
  if (stopped) return;
  stopped = true;
  scheduler?.stop();
  if (sweeper !== null) clearInterval(sweeper);
  if (flusher !== null) clearInterval(flusher);
  if (reconnectTimer !== null) clearTimeout(reconnectTimer);
  if (drainTimer !== null) clearTimeout(drainTimer);
  sweeper = flusher = reconnectTimer = drainTimer = null;
  flush();
  post({ type: "stopped" });
  try {
    link?.close();
  } catch {
    /* The transport may already have closed. */
  }
  link = null;
}

/* Stashes an evicted id so a late pong can still teach the estimator. */
function rememberEvicted(id: number, sent: number): void {
  graveyard.set(id, sent);
  if (graveyard.size > GRAVEYARD_MAX) {
    const oldest = graveyard.keys().next().value;
    if (oldest !== undefined) graveyard.delete(oldest);
  }
}

function record(sample: PingSample): void {
  outbox.push(sample);
  if (outbox.length >= MAX_BATCH_SAMPLES) flush();
}

function flush(): void {
  if (outbox.length === 0) return;
  const samples = outbox;
  outbox = [];
  post({ type: "samples", samples });
}
