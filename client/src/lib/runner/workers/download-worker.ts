/* ============================================================
 * The Graphite Meter: download read-and-count worker
 * ============================================================
 * One worker per parallel download stream. It streams GET /download, counts
 * each chunk's byteLength and discards it, so the payload never crosses the
 * thread boundary and memory stays O(1). Only `{ bytes }` deltas go back,
 * batched to ~50 ms. The lane re-fetches until the worker is terminated.
 * ============================================================ */

import { setDebugLogging, debugEnabled, dlog, DebugWindow } from "../../debug";
import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
import { nextTransferBytes, type SizerCfg } from "./autosize";
import { ProgressWindow, type ProgressDelta } from "./progressWindow";
import { tuned, DEFAULT_TUNING, type Tuning } from "./tuning";

/** Main → worker. `debug`/`id` drive verbose per-stream logging only. `chunk`
 *  selects the experimental mode: adaptively-sized `&bytes=N` requests (see
 *  autosize.ts) over one keep-alive connection, preserving cwnd. Default off
 *  runs one 64 GiB stream; the flag A/B-tests ramp responsiveness. The lane is
 *  stopped by terminating the worker, so there is no shutdown message. */
type InMsg =
  | {
      type: "start";
      url: string;
      debug?: boolean;
      id?: number;
      chunk?: boolean;
      credentials?: RequestCredentials;
      headers?: HeadersInit;
      tune?: Partial<Tuning>;
    }
  | { type: "measure"; seq: number };
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

/** Per-request controller, supplying `fetch` with a signal. Nothing aborts it:
 *  the lane is stopped by terminating the worker, which drops the request. */
let abort: AbortController | null = null;
/** Chunked mode (experimental) + its closed-loop state. */
let chunked = false;
let nextBytes = CHUNK_SIZER.minBytes;
let rateEwma = 0;
let measureSeq = 0;
/** Reusing one BYOB buffer is the only backpressure lever fetch exposes: without
 *  it Firefox reads far ahead into its own buffers, inflating RAM and undercounting. */
let tuning = tuned();
let progress = new ProgressWindow(0, tuning.reportGapMs);

/** Stream index, tagging debug lines only (`dl-worker#<id>`). */
let streamId = 0;
/** Raw-receive debug window, independent of the 50 ms progress batching.
 *  Reflects what this reader pulls off the socket, comparable to btop and
 *  `-verbose`. */
const dbg = new DebugWindow();

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    setDebugLogging(msg.debug ?? false);
    streamId = msg.id ?? 0;
    chunked = msg.chunk ?? false;
    // Folded to DEFAULT_TUNING unless the build opts into the bench surface
    // (GM_CLIENT_BENCH=1), which also eliminates the merge and msg.tune.
    tuning = __GM_BENCH__ ? tuned(msg.tune) : DEFAULT_TUNING;
    progress = new ProgressWindow(performance.now(), tuning.reportGapMs);
    credentials = msg.credentials ?? "same-origin";
    headers = msg.headers;
    nextBytes = CHUNK_SIZER.minBytes;
    rateEwma = 0;
    measureSeq = 0;
    progress.reset();
    dbg.reset();
    void run(msg.url);
  } else if (msg.type === "measure") {
    measureSeq = msg.seq;
    progress.reset();
  }
};

const post = (m: OutMsg) => ctx.postMessage(m);

function postProgress(delta: ProgressDelta | null): void {
  if (delta) post({ type: "progress", ...delta, seq: measureSeq });
}

async function run(url: string): Promise<void> {
  // Re-fetch loop: keep the lane busy for the whole measured window even if a
  // single request reaches its Content-Length.
  for (;;) {
    abort = new AbortController();
    // Count the chunk and drop it. Deltas batch to the main thread; verbose mode
    // logs the pre-aggregation 1 Hz receive rate, ground truth for the reader.
    const count = (n: number): void => {
      const now = performance.now();
      postProgress(progress.add(n, now));
      if (debugEnabled()) {
        const window = dbg.add(n, now);
        if (window) dlog(`dl-worker#${streamId}`, "raw-receive", window);
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
      postProgress(progress.flush()); // the window's remainder
      if (chunked) {
        ({ bytes: nextBytes, ewma: rateEwma } = nextTransferBytes(
          requestedBytes,
          performance.now() - fetchStart,
          rateEwma,
          CHUNK_SIZER,
        ));
      }
    } catch (err) {
      // A read that failed on an expired session is an auth failure, not a
      // transport one, so the session is re-checked before the error is reported.
      if (
        credentials === "include" &&
        (await sessionAuthenticationRequired(
          self.location.origin,
          abort.signal,
        ))
      ) {
        post({ type: "auth-required" });
        return;
      }
      postProgress(progress.flush());
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
  if (tuning.reader !== "byob") return null;
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
    let buf = new ArrayBuffer(tuning.readBufBytes);
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
