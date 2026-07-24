/* ============================================================
 * The Graphite Meter: upload generate-and-POST worker
 * ============================================================
 * One worker per parallel upload stream. It builds one incompressible Blob pool
 * from CSPRNG bytes, which gzip and br cannot shrink, and POSTs zero-copy slices
 * of it in a loop over plain HTTP/1.1. The server drains and counts the bytes;
 * upload-progress-worker.ts relays the authoritative total. This lane saturates.
 * ============================================================ */

import {
  setDebugLogging,
  debugEnabled,
  dlog,
  fmtRate,
  fmtBytes,
  fmtMs,
} from "../../debug";
import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
import { nextTransferBytes, type SizerCfg } from "./autosize";

/** `debug`/`id` drive verbose per-stream logging only. */
type InMsg =
  | {
      type: "start";
      url: string;
      debug?: boolean;
      id?: number;
      streams?: number;
      credentials?: RequestCredentials;
      headers?: Record<string, string>;
    }
  | { type: "stop" };
/** `alive` marks one POST the server drained, proving the lane is live. It
 *  carries no byte count: fetch has no upload-progress events, and the
 *  /upload/progress stream is the authoritative source. `error` restarts a lane. */
type OutMsg =
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "auth-required" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
let credentials: RequestCredentials = "same-origin";
let headers: Record<string, string> = {};
const post = (m: OutMsg) => ctx.postMessage(m);

/** One upload reservoir budget divided across the active lanes. */
const UPLOAD_TOTAL_POOL_BYTES = 64 * 1024 * 1024;
/** Pool floor keeps the autosizer useful on constrained devices. */
const MIN_POOL_BYTES = 2 * 1024 * 1024;

/* ---- Closed-loop POST sizing, per worker (see autosize.ts) ---- */
/** Wall-time each POST aims to span. The lower bound is about ACCURACY: the
 *  request/response turnaround stays inside the server's elapsed-time
 *  denominator, so a too-short POST lowers the measured rate. 500 ms keeps that
 *  fraction small, and interleaved lanes cover each other's turnaround. */
const TARGET_POST_MS = 500;
/** Smallest POST. Below this the per-request HTTP overhead dominates; it is also
 *  the size a freshly-dropped link converges down to within a few POSTs. */
const MIN_POST_BYTES = 128 * 1024;
/** Sizer tuning shared with autosize.ts (maxBytes is the pool size, set on `start`). */
const sizer: SizerCfg = {
  targetMs: TARGET_POST_MS,
  minBytes: MIN_POST_BYTES,
  maxBytes: MIN_POST_BYTES, // raised to the pool size in onmessage(start)
  alpha: 0.3,
  stepUp: 2,
  stepDown: 0.5,
};
/** The pool is built by repeating this one filled block, so construction peaks
 *  at ~block + pool. That bound is what keeps a single-stream run inside iOS
 *  Safari's tab-kill threshold. */
const FILL_BLOCK_BYTES = 4 * 1024 * 1024;
/** crypto.getRandomValues' hard per-call byte quota. */
const RNG_CHUNK_BYTES = 65536;

/** Divide the device-scaled total reservoir across the actual lane count. */
export function uploadPoolBytes(
  streams: number,
  deviceMemory?: number,
): number {
  streams = Math.max(1, streams);
  if (typeof deviceMemory === "number") {
    if (deviceMemory <= 2)
      return Math.max(MIN_POOL_BYTES, Math.floor((16 * 1024 * 1024) / streams));
    if (deviceMemory <= 4)
      return Math.max(MIN_POOL_BYTES, Math.floor((24 * 1024 * 1024) / streams));
  }
  return Math.max(
    MIN_POOL_BYTES,
    Math.floor(UPLOAD_TOTAL_POOL_BYTES / streams),
  );
}

/** Whether retrying the lane after a non-OK POST is worthwhile. 429 (rate
 *  limited), 413 (too large), 503 (unavailable) and 410 (gone) are terminal for
 *  this run: re-POSTing hammers a server that will not take it. Everything else,
 *  including 500 and any network or abort error, counts as transient. */
export function recoverableStatus(status: number): boolean {
  return !(
    status === 429 ||
    status === 413 ||
    status === 503 ||
    status === 410
  );
}

let stopped = false;
/** Aborts the in-flight POST on `stop` (mirrors download-worker.ts). */
let abort: AbortController | null = null;
/** The reused incompressible pool Blob, built once on first start. Each POST is
 *  a zero-copy `pool.slice`, so fetch references the pool's backing store. An
 *  ArrayBuffer body copies on every call instead, churning gigabytes/sec on a
 *  fast link, faster than GC reclaims it. */
let pool: Blob | null = null;
/** The 4 MiB incompressible source block, filled once with CSPRNG bytes and
 *  repeated to build the pool (caps the construction-time heap peak). Typed over
 *  ArrayBuffer (not the default ArrayBufferLike) so it is a valid BlobPart. */
let fillBlock: Uint8Array<ArrayBuffer> | null = null;
/** Byte length of the pool as actually built. */
let poolBytes = 0;
/** Per-lane pool size to build, device-bounded so a phone cannot OOM. Also the
 *  autosizer's upper clamp. */
let poolTargetBytes = UPLOAD_TOTAL_POOL_BYTES;
/** Bytes the NEXT POST sends, the closed-loop variable. Starts at MIN for a fast
 *  first sample, then tracks TARGET_POST_MS × this lane's smoothed rate. */
let nextBytes = MIN_POST_BYTES;
/** This lane's smoothed throughput (bytes/sec); 0 until the first POST completes. */
let rateEwma = 0;

/** Stream index, tagging debug lines only (`ul-worker#<id>`). */
let streamId = 0;
/** Completed-POST debug window: server-drained bytes since the last 1 Hz log,
 *  its start time, and the running per-stream total. One step per POST rather
 *  than byte-granular, enough to show whether turnaround leaves the wire idle. */
let dbgWinBytes = 0;
let dbgWinStart = 0;
let dbgTotal = 0;

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    stopped = false;
    setDebugLogging(msg.debug ?? false);
    streamId = msg.id ?? 0;
    credentials = msg.credentials ?? "same-origin";
    headers = msg.headers ?? {};
    const deviceMemory = (navigator as unknown as { deviceMemory?: number })
      .deviceMemory;
    poolTargetBytes = uploadPoolBytes(msg.streams ?? 1, deviceMemory);
    sizer.maxBytes = poolTargetBytes; // the pool is the size ceiling
    nextBytes = Math.min(MIN_POST_BYTES, poolTargetBytes);
    rateEwma = 0;
    dbgWinBytes = 0;
    dbgTotal = 0;
    dbgWinStart = performance.now();
    void run(msg.url);
  } else if (msg.type === "stop") {
    stopped = true;
    abort?.abort();
  }
};

/** The reusable 4 MiB incompressible source block, filled once with CSPRNG bytes
 *  in 64 KiB chunks (the getRandomValues per-call quota). Reused for every
 *  payload, so the fill cost is paid once and never on the POST hot path. */
function incompressibleBlock(): Uint8Array<ArrayBuffer> {
  if (fillBlock) return fillBlock;
  const b = new Uint8Array(new ArrayBuffer(FILL_BLOCK_BYTES));
  for (let off = 0; off < b.length; off += RNG_CHUNK_BYTES) {
    crypto.getRandomValues(
      b.subarray(off, Math.min(off + RNG_CHUNK_BYTES, b.length)),
    );
  }
  fillBlock = b;
  return b;
}

/** Build the reused pool by repeating one filled block up to poolTargetBytes.
 *  The Blob copies each part into its own backing store, so the construction
 *  heap peaks at ~block + pool. Every POST then slices a view of it. */
function buildPool(): void {
  if (pool && poolBytes === poolTargetBytes) return;
  const block = incompressibleBlock();
  const parts: BlobPart[] = [];
  let remaining = poolTargetBytes;
  while (remaining > 0) {
    const take = Math.min(remaining, block.byteLength);
    parts.push(take === block.byteLength ? block : block.subarray(0, take));
    remaining -= take;
  }
  pool = new Blob(parts, { type: "application/octet-stream" });
  poolBytes = poolTargetBytes;
}

/** Drain the tiny JSON echo so the keep-alive connection serves the next POST:
 *  an unread body pins it and stalls the lane. The POST is already complete, so
 *  a failed drain costs at most one connection. */
async function drainForKeepAlive(res: Response): Promise<void> {
  await res.arrayBuffer().catch(() => {});
}

/** POST adaptively-sized slices of the pool in a loop to keep the lane saturated
 *  for the whole stage. Mirrors download-worker.ts's re-fetch loop: a fresh
 *  AbortController per POST, `stop` aborts it, a network error ends the lane
 *  (RealBackend restarts it). Each completed POST resizes the NEXT one. */
async function run(url: string): Promise<void> {
  if (stopped) return;
  buildPool();
  if (!pool) return;

  while (!stopped) {
    abort = new AbortController();
    const sentBytes = nextBytes;
    const postStart = performance.now();
    try {
      const res = await fetch(url, {
        method: "POST",
        body: pool.slice(0, sentBytes), // zero-copy view of the pool
        signal: abort.signal,
        cache: "no-store",
        headers: { ...headers, "Content-Type": "application/octet-stream" },
        credentials,
        redirect: redirectForCredentials(credentials),
      });
      if (authenticationRequired(res)) {
        post({ type: "auth-required" });
        return;
      }
      await drainForKeepAlive(res);
      if (!res.ok) {
        post({
          type: "error",
          recoverable: recoverableStatus(res.status),
          detail: `HTTP ${res.status}`,
        });
        return; // RealBackend decides whether to restart this lane
      }
      // The server drained a full slice: the lane is alive. No bytes travel here,
      // /upload/progress is authoritative; this only resets the restart counter.
      post({ type: "alive" });
      ({ bytes: nextBytes, ewma: rateEwma } = nextTransferBytes(
        sentBytes,
        performance.now() - postStart,
        rateEwma,
        sizer,
      ));
      if (debugEnabled()) {
        dbgWinBytes += sentBytes;
        dbgTotal += sentBytes;
        const now = performance.now();
        const dt = now - dbgWinStart;
        if (dt >= 1000) {
          dlog(`ul-worker#${streamId}`, "post-complete", {
            rate: fmtRate(dbgWinBytes / (dt / 1000)),
            postSize: fmtBytes(nextBytes),
            window: fmtBytes(dbgWinBytes),
            total: fmtBytes(dbgTotal),
            dt: fmtMs(dt),
          });
          dbgWinBytes = 0;
          dbgWinStart = now;
        }
      }
    } catch (err) {
      if (stopped) return; // stop() aborted it: a clean teardown
      if (
        credentials === "include" &&
        (await sessionAuthenticationRequired(
          self.location.origin,
          abort.signal,
        ))
      ) {
        stopped = true;
        post({ type: "auth-required" });
        return;
      }
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}
