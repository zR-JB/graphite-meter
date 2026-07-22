/* ============================================================
 * The Graphite Meter — Upload generate-and-POST worker (Stage 3)
 * ============================================================
 *
 * One worker per parallel upload stream. It builds ONE incompressible Blob "pool"
 * with `crypto.getRandomValues` (filled once; CSPRNG bytes are incompressible to
 * gzip/br, so incompressibility holds) and
 * POSTs a zero-copy `pool.slice(0, n)` of it in a loop over plain HTTP/1.1 via
 * `fetch`. The SERVER drains + counts the bytes and relays the authoritative count
 * over /upload/progress (see upload-progress-worker.ts); this worker just keeps the lane
 * saturated and is otherwise measurement-blind.
 *
 * ── Why the POST size adapts (dial-up → multi-Gbit in one tool) ──
 * A fixed POST size can't span that range: huge on a slow link (a giant in-flight
 * payload, coarse measurement, sluggish response when the line drops mid-test);
 * pinned-too-small everywhere else. So each POST is sized closed-loop to a ~500 ms
 * wall-target from THIS lane's own observed rate (an EWMA) — purely per-worker, so
 * lanes never synchronise into oscillation. There are NO preemptive kills: the
 * current POST always finishes; only the NEXT one is resized (a step-clamp is the
 * hysteresis). The size rides between MIN_POST_BYTES and the pool size (a fixed
 * per-lane reservoir).
 *
 * ── Why a fixed-Blob `fetch` (and NOT a streaming fetch) ──
 * A `fetch` whose body is a *ReadableStream* (`duplex:'half'`) is the streaming
 * upload primitive — and it requires HTTP/2 in Chrome (→ ALPN failure on our
 * cleartext h1.1 origin) and is unimplemented in Firefox. That path is NEVER used
 * here. A `fetch` whose body is a *fixed Blob* has a known Content-Length and is
 * an ordinary h1.1 request that works in every target browser, with the same
 * abort + re-loop shape as the download worker and a real `res.ok`/`res.status`
 * so a 4xx/5xx is handled instead of blindly re-POSTed.
 *
 * ── Why no progress events (server-authoritative) ──
 * `fetch` has no upload-progress events, and none are needed: the upload figure
 * is the SERVER's drained byte count (the only count downstream of every
 * browser/proxy send buffer — it can lag the wire but never lead it). So this
 * worker reports only lane liveness: one `{type:'alive'}` per completed POST
 * (proving the lane recovered, for the restart logic) and `{type:'error'}` on a
 * failed POST. It NEVER reports bytes — the /upload/progress count is the sole source.
 * The 100 ms server frames carry the authoritative byte/time snapshots; a
 * dropped progress stream reconnects without removing that gap from the result.
 *
 * ── Why fetch here mirrors download-worker.ts ──
 * Download = fetch + body.getReader(): read-and-DISCARD a streamed RESPONSE at
 * O(1) memory. Upload = fetch + fixed Blob body: stream a generated REQUEST from
 * one Blob and read nothing back (a tiny JSON echo, drained to free the keep-alive
 * connection). Same fetch/abort/re-loop skeleton both directions.
 *
 * Message protocol (RealBackend's pool drives both directions):
 *   in:  { type: 'start', url, debug?, id? } | { type: 'stop' }
 *   out: { type: 'alive' } | { type: 'error', recoverable, detail }
 *
 * ── Why a Blob pool + slice and not a per-POST ArrayBuffer ──
 * `fetch(body: arrayBuffer)` (like `xhr.send`) COPIES the body bytes every call, so
 * looping POSTs of a freshly-built multi-MiB body churn a copy per request — on a
 * fast (loopback) link that copied at gigabytes/sec, faster than GC reclaimed it,
 * ballooning the heap to many GB. We build the incompressible pool Blob ONCE and
 * `pool.slice(0, n)` per POST: a Blob slice REFERENCES the pool's backing store (a
 * view with an offset/length — no byte copy), and fetch streams from it straight to
 * the socket. The footprint stays flat regardless of how the size adapts. NEVER
 * rebuild a Blob per POST.
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
/** `alive` = one POST drained by the server (lane is live; NO byte count — the
 *  /upload/progress stream carries the authoritative count). `error` drives lane restart. */
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

/* ---- Closed-loop POST sizing (per-worker, no kills; see autosize.ts) ---- */
/** Wall-time each POST aims to span. The lower bound matters for ACCURACY, not just
 *  overhead: request→response turnaround remains in the server's elapsed-time
 *  denominator, so a too-short POST makes overhead a large fraction of measured
 *  time and correctly lowers the rate. 500 ms keeps it small across the range
 *  where the sizer is below the pool ceiling; the default ≥3 interleaved lanes
 *  cover the rest (while one lane turns around, the others keep the clock advancing).
 *  Still short enough to bound in-flight + re-measure within ~½ s on a slow/dropping
 *  link. (On a fast link the POST clamps to the pool size and drains faster than this.) */
const TARGET_POST_MS = 500;
/** Smallest POST. Below this the per-request HTTP overhead dominates; it is also
 *  the size a freshly-dropped link converges down to within a few POSTs. */
const MIN_POST_BYTES = 128 * 1024;
/** Sizer tuning shared with autosize.ts (MAX is the pool size, set on `start`). */
const SIZER: SizerCfg = {
  targetMs: TARGET_POST_MS,
  minBytes: MIN_POST_BYTES,
  maxBytes: MIN_POST_BYTES, // raised to the pool size in onmessage(start)
  alpha: 0.3,
  stepUp: 2,
  stepDown: 0.5,
};
/** The payload is built by repeating ONE filled block, so the peak transient heap
 *  during construction is ~block + payload, not the 2× a fresh Uint8Array(bufBytes)
 *  plus its Blob copy would cost (the iOS-Safari tab-kill guard at 1 stream). */
const FILL_BLOCK_BYTES = 4 * 1024 * 1024;
/** crypto.getRandomValues' hard per-call byte quota. */
const RNG_CHUNK_BYTES = 65536;

/** Divide the device-scaled total reservoir across the actual lane count. */
export function uploadPoolBytes(
  streams: number,
  deviceMemory?: number,
): number {
  streams = Math.max(1, streams);
  const dm = deviceMemory;
  if (typeof dm === "number") {
    if (dm <= 2)
      return Math.max(MIN_POOL_BYTES, Math.floor((16 * 1024 * 1024) / streams));
    if (dm <= 4)
      return Math.max(MIN_POOL_BYTES, Math.floor((24 * 1024 * 1024) / streams));
  }
  return Math.max(
    MIN_POOL_BYTES,
    Math.floor(UPLOAD_TOTAL_POOL_BYTES / streams),
  );
}

/** Map a non-OK POST status to whether retrying the lane is worthwhile.
 *  429 (rate-limited) / 413 (too large) / 503 (unavailable) / 410 (gone) are
 *  terminal for this run — re-POSTing just hammers a server that won't take it.
 *  Everything else (incl. 500 and any network/abort error) is treated transient. */
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
/** The reused incompressible pool Blob (built once on first start). Each POST is a
 *  zero-copy slice of it — NOT a fresh Blob/ArrayBuffer — so fetch references the
 *  pool's backing store instead of copying. */
let pool: Blob | null = null;
/** The 4 MiB incompressible source block, filled once with CSPRNG bytes and
 *  repeated to build the pool (caps the construction-time heap peak). Typed over
 *  ArrayBuffer (not the default ArrayBufferLike) so it is a valid BlobPart. */
let fillBlock: Uint8Array<ArrayBuffer> | null = null;
/** The pool's byte length = the autosizer's MAX. */
let poolBytes = 0;
/** Per-lane pool size, device-bounded so a phone cannot OOM. Also the autosizer's
 *  upper clamp. */
let bufBytes = UPLOAD_TOTAL_POOL_BYTES;
/** Bytes the NEXT POST will send — the closed-loop variable. Starts at MIN for a
 *  fast first sample, then tracks TARGET_POST_MS × this lane's smoothed rate. */
let nextBytes = MIN_POST_BYTES;
/** This lane's smoothed throughput (bytes/sec); 0 until the first POST completes. */
let rateEwma = 0;

/** Stream index, only used to tag debug lines (`ul-worker#<id>`). */
let streamId = 0;
/** Completed-POST debug window: bytes fully POSTed (server-drained) since the last
 *  1 Hz log + its start time + the running per-stream total. One step per POST,
 *  not byte-granular, but it still shows whether the request→response
 *  turnaround is leaving the wire idle. */
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
    bufBytes = uploadPoolBytes(msg.streams ?? 1, deviceMemory);
    SIZER.maxBytes = bufBytes; // the pool is the size ceiling
    nextBytes = Math.min(MIN_POST_BYTES, bufBytes);
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

/** Build the reused pool by REPEATING one filled block up to bufBytes. The Blob
 *  copies each part into its own backing store, so the construction-time heap peak
 *  is ~block + pool (not the 2× a fresh Uint8Array(bufBytes) + its Blob copy would
 *  cost). Every POST then slices a view of it — no per-POST copy. */
function buildPool(): void {
  if (pool && poolBytes === bufBytes) return;
  const block = incompressibleBlock();
  const parts: BlobPart[] = [];
  let remaining = bufBytes;
  while (remaining > 0) {
    const take = Math.min(remaining, block.byteLength);
    parts.push(take === block.byteLength ? block : block.subarray(0, take));
    remaining -= take;
  }
  pool = new Blob(parts, { type: "application/octet-stream" });
  poolBytes = bufBytes;
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
      if (
        res.status === 403 &&
        res.headers.get("Graphite-Meter-Auth") === "required"
      ) {
        post({ type: "auth-required" });
        return;
      }
      // Drain the tiny JSON echo so this keep-alive connection is reusable for the
      // next POST (an unread body can pin the connection and stall the lane).
      await res.arrayBuffer().catch(() => {});
      if (!res.ok) {
        post({
          type: "error",
          recoverable: recoverableStatus(res.status),
          detail: `HTTP ${res.status}`,
        });
        return; // RealBackend decides whether to restart this lane
      }
      // One full slice was drained by the server: the lane is alive. NO bytes — the
      // /upload/progress count is authoritative; this only resets the restart counter.
      post({ type: "alive" });
      ({ bytes: nextBytes, ewma: rateEwma } = nextTransferBytes(
        sentBytes,
        performance.now() - postStart,
        rateEwma,
        SIZER,
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
      if (stopped) return; // aborted by stop() — a clean teardown, not an error
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
