/* ============================================================
 * The Graphite Meter: download read-and-count worker
 * ============================================================
 * One worker per parallel download stream. It streams GET /download, counts
 * each chunk's byteLength and discards it, so the payload never crosses the
 * thread boundary and memory stays O(1). Only `{ bytes }` deltas go back,
 * batched to ~50 ms. The lane re-fetches until `stop` aborts the fetch.
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

/** Main → worker. `debug`/`id` drive verbose per-stream logging only. `chunk`
 *  selects the experimental mode: adaptively-sized `&bytes=N` requests (see
 *  autosize.ts) over one keep-alive connection, preserving cwnd. Default off
 *  runs one 64 GiB stream; the flag A/B-tests ramp responsiveness. */
type InMsg =
  | {
      type: "start";
      url: string;
      debug?: boolean;
      id?: number;
      chunk?: boolean;
      credentials?: RequestCredentials;
      headers?: HeadersInit;
    }
  | { type: "measure"; seq: number }
  | { type: "stop" };
/** Worker → main. */
type OutMsg =
  | { type: "progress"; bytes: number; elapsedMs: number; seq: number }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "auth-required" };

export function recoverableDownloadStatus(status: number): boolean {
  return status !== 429 && status !== 503;
}

export function downloadFetchInit(
  signal: AbortSignal,
  requestCredentials: RequestCredentials,
  requestHeaders?: HeadersInit,
): RequestInit {
  return {
    signal,
    cache: "no-store",
    credentials: requestCredentials,
    headers: requestHeaders,
    redirect: redirectForCredentials(requestCredentials),
  };
}

// Narrow `self` to the dedicated-worker scope so postMessage/onmessage type
// cleanly under the combined DOM + WebWorker libs.
const ctx = self as unknown as DedicatedWorkerGlobalScope;
let credentials: RequestCredentials = "same-origin";
let headers: HeadersInit | undefined;

/** Post a delta no more often than this (ms); flushed on stream end / stop. */
const POST_INTERVAL_MS = 50;

/** Size of the single reused BYOB read buffer, one per worker for the whole
 *  stage. Firefox pulls far ahead of the reader into its own buffers when the
 *  link outruns this loop (loopback), inflating process RAM and undercounting.
 *  Reusing one buffer is the only backpressure lever fetch exposes. */
const READ_BUF_BYTES = 1024 * 1024; // 1 MiB

/** Experimental chunked-request sizer (see autosize.ts). The generous max costs
 *  no RAM because the reader discards as it counts, so a fast stable link climbs
 *  toward it and only a slow or dropping link shrinks for responsiveness. */
const CHUNK_SIZER: SizerCfg = {
  targetMs: 350,
  minBytes: 128 * 1024,
  maxBytes: 256 * 1024 * 1024,
  alpha: 0.3,
  stepUp: 2,
  stepDown: 0.5,
};

let abort: AbortController | null = null;
let stopped = false;
/** Chunked mode (experimental) + its closed-loop state. */
let chunked = false;
let nextBytes = CHUNK_SIZER.minBytes;
let rateEwma = 0;
let measureSeq = 0;
/** Bytes counted since the last posted delta, and when that window opened. */
let windowBytes = 0;
let windowStart = 0;

/** Stream index, tagging debug lines only (`dl-worker#<id>`). */
let streamId = 0;
/** Raw-receive debug window, independent of the 50 ms progress batching: bytes
 *  since the last 1 Hz log, its start time, and the per-stream total. Reflects
 *  what this reader pulls off the socket, comparable to btop and `-verbose`. */
let dbgWinBytes = 0;
let dbgWinStart = 0;
let dbgTotal = 0;

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    stopped = false;
    setDebugLogging(msg.debug ?? false);
    streamId = msg.id ?? 0;
    chunked = msg.chunk ?? false;
    credentials = msg.credentials ?? "same-origin";
    headers = msg.headers;
    nextBytes = CHUNK_SIZER.minBytes;
    rateEwma = 0;
    measureSeq = 0;
    resetProgressWindow();
    dbgWinBytes = 0;
    dbgTotal = 0;
    dbgWinStart = performance.now();
    void run(msg.url);
  } else if (msg.type === "measure") {
    measureSeq = msg.seq;
    resetProgressWindow();
  } else if (msg.type === "stop") {
    stopped = true;
    abort?.abort();
  }
};

const post = (m: OutMsg) => ctx.postMessage(m);

function resetProgressWindow(): void {
  windowBytes = 0;
  windowStart = performance.now();
}

function flushProgress(now = performance.now()): void {
  if (windowBytes <= 0) {
    windowStart = now;
    return;
  }
  const elapsedMs = now - windowStart;
  if (elapsedMs > 0)
    post({ type: "progress", bytes: windowBytes, elapsedMs, seq: measureSeq });
  windowBytes = 0;
  windowStart = now;
}

async function run(url: string): Promise<void> {
  // Re-fetch loop: keep the lane busy for the whole measured window even if a
  // single request reaches its Content-Length.
  while (!stopped) {
    abort = new AbortController();
    let lastPost = performance.now();
    // Count the chunk and drop it. Deltas batch to the main thread; verbose mode
    // logs the pre-aggregation 1 Hz receive rate, ground truth for the reader.
    const count = (n: number): void => {
      windowBytes += n;
      const now = performance.now();
      if (now - lastPost >= POST_INTERVAL_MS) {
        flushProgress(now);
        lastPost = now;
      }
      if (debugEnabled()) {
        dbgWinBytes += n;
        dbgTotal += n;
        const dt = now - dbgWinStart;
        if (dt >= 1000) {
          dlog(`dl-worker#${streamId}`, "raw-receive", {
            rate: fmtRate(dbgWinBytes / (dt / 1000)),
            window: fmtBytes(dbgWinBytes),
            total: fmtBytes(dbgTotal),
            dt: fmtMs(dt),
          });
          dbgWinBytes = 0;
          dbgWinStart = now;
        }
      }
    };
    // Chunked mode appends the adaptive size; long-stream mode uses the URL as-is
    // (its ?bytes= is baked in by RealBackend). Time the whole fetch to resize next.
    const requestedBytes = nextBytes;
    const requestUrl = chunked ? `${url}&bytes=${requestedBytes}` : url;
    const fetchStart = performance.now();
    try {
      const res = await fetch(
        requestUrl,
        downloadFetchInit(abort.signal, credentials, headers),
      );
      if (authenticationRequired(res)) {
        post({ type: "auth-required" });
        return;
      }
      if (!res.ok || !res.body) {
        post({
          type: "error",
          recoverable: recoverableDownloadStatus(res.status),
          detail: `HTTP ${res.status}`,
        });
        return;
      }
      await readBody(res.body, count);
      flushProgress(); // the window's remainder
      if (chunked) {
        ({ bytes: nextBytes, ewma: rateEwma } = nextTransferBytes(
          requestedBytes,
          performance.now() - fetchStart,
          rateEwma,
          CHUNK_SIZER,
        ));
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
      flushProgress();
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}

/** A BYOB reader over the body, or null when the body is not a byte stream.
 *  Reusing one buffer avoids the default reader's per-chunk allocation and GC,
 *  which is the read-side throughput ceiling at multi-Gbit/s. */
function byobReader(
  body: ReadableStream<Uint8Array>,
): ReadableStreamBYOBReader | null {
  try {
    return body.getReader({ mode: "byob" });
  } catch {
    return null;
  }
}

/** Read a response body to completion, feeding each chunk's byte count to
 *  `count`. fetch plus a reader is the only way to read and discard a streamed
 *  response at O(1) memory: XHR buffers the whole response internally, which a
 *  multi-GiB download exhausts. */
async function readBody(
  body: ReadableStream<Uint8Array>,
  count: (n: number) => void,
): Promise<void> {
  const byob = byobReader(body);
  if (byob) {
    let buf = new ArrayBuffer(READ_BUF_BYTES);
    for (;;) {
      const chunk = await byob.read(new Uint8Array(buf));
      if (chunk.done) break;
      if (chunk.value.byteLength) count(chunk.value.byteLength);
      // read() detaches the buffer and returns the same backing store in
      // `value`; reusing it keeps the loop allocation-free.
      buf = chunk.value.buffer as ArrayBuffer;
    }
    return;
  }
  const reader = body.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    if (value) count(value.byteLength);
  }
}
