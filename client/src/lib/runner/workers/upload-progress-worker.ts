/* ============================================================
 * Upload progress worker — server-authoritative upload relay
 * Owns /ws/upload WebSocket on its own thread (never co-located
 * with saturated upload worker to avoid UI lag). Carries test's
 * server-minted ?id= and relays SERVER-measured drained byte count.
 *
 * Protocol (message-delimited ASCII via real/wire.ts):
 *   on open    → HI,ws; server replies READY (ignored)
 *   every 3s   → HI keepalive (server's 10s idle deadline)
 *   push       → BYTES_RECEIVED,<n> (~10 Hz, cumulative)
 *   on stop    → BYE; server replies UPLOAD_COMPLETE,<n>, close
 *
 * Counts cumulative & self-healing: dropped frame loses nothing
 * (next has corrected total). Dropped socket non-fatal: reconnects
 * with backoff; main thread keeps live needle until recovery.
 * ============================================================ */

import { encode, decode } from "../real/wire";

type InMsg = { type: "start"; url: string } | { type: "stop" };
// `t` is the server's ACTIVE measurement time (ns the server was actually draining
// bytes for this id, dead zones excluded) at which it sampled `n` — the client
// measures upload rate over this server clock (Δn / Δt), never its own arrival clock
// and never a wall span. Safe as a JS number: active ns stays < 2^53 for ~104 days.
type OutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "stall"; detail: string }
  | { type: "resume" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg): void => ctx.postMessage(m);

/** Re-send HI this often to keep the server's read side warm (< its 10 s idle). */
const KEEPALIVE_MS = 3000;
/** Reconnect backoff bounds (ms). */
const RECONNECT_MIN_MS = 100;
const RECONNECT_MAX_MS = 2000;
/** After BYE, wait at most this long for UPLOAD_COMPLETE before closing anyway. */
const BYE_GRACE_MS = 800;

let url = "";
let ws: WebSocket | null = null;
let stopped = false;
let stalledOut = false; // a `stall` emitted, not yet matched by `resume`
let backoff = 0;
/** Last cumulative count forwarded — guards against a non-monotonic frame. */
let lastN = 0;

let keepalive: ReturnType<typeof setInterval> | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let byeTimer: ReturnType<typeof setTimeout> | null = null;

ctx.onmessage = (e: MessageEvent<InMsg>): void => {
  const m = e.data;
  if (m.type === "start") {
    url = m.url;
    stopped = false;
    lastN = 0;
    connect();
  } else if (m.type === "stop") {
    finish();
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
    trySend(encode({ op: "HI", proto: "ws" }));
    keepalive ??= setInterval(() => trySend(encode({ op: "HI", proto: "ws" })), KEEPALIVE_MS);
  };
  ws.onmessage = (ev: MessageEvent): void => onFrame(ev.data);
  // onerror is always followed by onclose for WebSocket — reconnect once, in onclose.
  ws.onclose = (): void => onDisconnect("websocket closed");
}

function onFrame(data: unknown): void {
  if (typeof data !== "string") return; // the bus is text-only
  let f;
  try {
    f = decode(data);
  } catch {
    return; // ignore malformed / ERR / READY frames — never tear the bus down
  }
  if (f.op === "BYTES_RECEIVED") {
    const n = Number(f.n);
    if (n >= lastN) {
      lastN = n;
      post({ type: "bytes", n, t: Number(f.nanos) });
    }
    return;
  }
  if (f.op === "UPLOAD_COMPLETE") {
    const n = Number(f.n);
    if (n >= lastN) lastN = n;
    post({ type: "complete", n: lastN, t: Number(f.nanos) });
    teardown(); // the authoritative final arrived — close for good
  }
}

function onDisconnect(detail: string): void {
  if (stopped) return; // a normal close after BYE/teardown
  ws = null;
  scheduleReconnect(detail);
}

function scheduleReconnect(detail: string): void {
  if (stopped) return;
  if (!stalledOut) {
    post({ type: "stall", detail });
    stalledOut = true;
  }
  backoff = backoff === 0 ? RECONNECT_MIN_MS : Math.min(backoff * 2, RECONNECT_MAX_MS);
  reconnectTimer = setTimeout(connect, backoff);
}

/** Graceful finish on `stop`: send BYE so the server emits UPLOAD_COMPLETE, then
 *  close once it arrives (onFrame) or after a short grace. */
function finish(): void {
  if (stopped) return;
  stopped = true;
  trySend(encode({ op: "BYE" }));
  byeTimer = setTimeout(teardown, BYE_GRACE_MS);
}

function teardown(): void {
  stopped = true;
  if (keepalive !== null) clearInterval(keepalive);
  if (reconnectTimer !== null) clearTimeout(reconnectTimer);
  if (byeTimer !== null) clearTimeout(byeTimer);
  keepalive = reconnectTimer = byeTimer = null;
  try {
    ws?.close(1000, "");
  } catch {
    /* already closed */
  }
  ws = null;
}

function trySend(msg: string): void {
  try {
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(msg);
  } catch {
    /* closed mid-send — onclose drives the reconnect */
  }
}
