/* ============================================================
 * The Graphite Meter — Download read-and-count worker (Stage 2)
 * ============================================================
 *
 * One worker per parallel download stream. It opens a streaming fetch
 * against GET /download, reads the body chunk-by-chunk, and **counts
 * byteLength then discards the chunk** — the random payload is never kept
 * and never crosses the thread boundary. Only tiny `{ bytes }` deltas are
 * posted back (batched to ~50 ms), so the main thread aggregates many
 * streams without the read loop ever competing with gauge rendering.
 *
 * The lane stays saturated for the whole stage: if the server stream ends
 * naturally (Content-Length reached) the worker re-fetches until stopped.
 * `stop` aborts the in-flight fetch. A recoverable failure is reported so
 * the main thread can stall + restart this single lane.
 *
 * ── Why fetch here, but XHR in upload-worker.ts (intentional asymmetry) ──
 * The two directions use different APIs because the platform forces it, not by
 * accident:
 *   • Download = fetch + body.getReader(): the only way to read-and-DISCARD a
 *     streamed response at O(1) memory. XHR buffers the whole response
 *     internally (responseText/response), so a multi-GiB download test would
 *     OOM — XHR-for-download is a non-starter. We use a BYOB reader reusing ONE
 *     buffer (see readBody): at multi-Gbit/s the default reader's per-chunk
 *     Uint8Array allocation + GC is the read-side ceiling — and the reason the
 *     JS reader couldn't keep up with the wire (so the link buffered ahead and
 *     the kernel/btop counter ran higher than what the app actually consumed,
 *     most visibly in Firefox). Reusing the buffer removes that ceiling.
 *   • Upload = XHR: we need upload.onprogress to count bytes ACTUALLY sent, and
 *     fetch has no upload-progress events at all; fetch *streaming* upload
 *     additionally requires HTTP/2 (dead end on our cleartext h1.1 origin).
 * The worker message protocol is identical both ways, so RealBackend's pool
 * treats them uniformly. WebTransport (Stage 5) is the truly-symmetric path.
 *
 * ── Firefox download RAM caveat (known, documented, not a bug we can fix) ──
 * When the LINK is faster than this read loop (loopback / fast LAN), Firefox
 * pulls bytes off the socket into its own internal stream buffers far ahead of
 * what getReader() has drained, before its high-water mark engages backpressure.
 * On a ~40 Gbit/s loopback that lookahead buffer is huge and slow to saturate,
 * so the Firefox PROCESS RAM balloons (10+ GB) and the app's counted rate runs
 * BELOW what btop/the server see — the missing bytes are sitting in Firefox's
 * buffer, not lost. We already do everything fetch exposes for backpressure
 * (one reused BYOB buffer; we only read as fast as we count), so there is no JS
 * lever left. It only manifests when the BROWSER is the bottleneck; on any real
 * internet path the LINE is the bottleneck, the reader keeps up, and the gap
 * never appears. Chrome buffers far less and does not show it. See
 * docs/THROUGHPUT_MEASUREMENT.md.
 *
 * Only dependency is the shared debug logger (gated; silent unless the dev
 * flag is on), so it still bundles cleanly as a Vite module worker.
 * ============================================================ */

import { setDebugLogging, debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../../debug";

/** Main → worker. `debug`/`id` drive verbose per-stream logging only. */
type InMsg = { type: "start"; url: string; debug?: boolean; id?: number } | { type: "stop" };
/** Worker → main. */
type OutMsg =
  | { type: "progress"; bytes: number }
  | { type: "error"; recoverable: boolean; detail: string };

// Narrow `self` to the dedicated-worker scope so postMessage/onmessage type
// cleanly under the combined DOM + WebWorker libs.
const ctx = self as unknown as DedicatedWorkerGlobalScope;

/** Post a delta no more often than this (ms); flushed on stream end / stop. */
const POST_INTERVAL_MS = 50;

/** Size of the single reused BYOB read buffer. Large enough that one read can
 *  return a big slice (fewer read() turns per second), small enough to stay
 *  lean — it's one buffer per worker, reused for the whole stage. */
const READ_BUF_BYTES = 1024 * 1024; // 1 MiB

let abort: AbortController | null = null;
let stopped = false;

/** Stream index, only used to tag debug lines (`dl-worker#<id>`). */
let streamId = 0;
/** Raw-receive debug window: bytes since the last 1 Hz log + its start time +
 *  the running per-stream total. Independent of the 50 ms progress batching, so
 *  it reflects exactly what THIS reader pulls off the socket — the figure to
 *  compare against btop and the server `-verbose` log. */
let dbgWinBytes = 0;
let dbgWinStart = 0;
let dbgTotal = 0;

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    stopped = false;
    setDebugLogging(msg.debug ?? false);
    streamId = msg.id ?? 0;
    dbgWinBytes = 0;
    dbgTotal = 0;
    dbgWinStart = performance.now();
    void run(msg.url);
  } else if (msg.type === "stop") {
    stopped = true;
    abort?.abort();
  }
};

const post = (m: OutMsg) => ctx.postMessage(m);

async function run(url: string): Promise<void> {
  // Re-fetch loop: keep the lane busy for the whole measured window even if a
  // single request reaches its Content-Length.
  while (!stopped) {
    abort = new AbortController();
    let acc = 0;
    let lastPost = performance.now();
    // Count a chunk's bytes (the chunk itself is dropped): batch deltas to the
    // main thread (~50 ms) and, when verbose, log the raw 1 Hz receive rate —
    // BEFORE any aggregation/EMA, the ground truth for "did the data reach JS?".
    const count = (n: number): void => {
      acc += n;
      const now = performance.now();
      if (now - lastPost >= POST_INTERVAL_MS) {
        post({ type: "progress", bytes: acc });
        acc = 0;
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
    try {
      const res = await fetch(url, { signal: abort.signal, cache: "no-store" });
      if (!res.ok || !res.body) {
        post({ type: "error", recoverable: true, detail: `HTTP ${res.status}` });
        return;
      }
      await readBody(res.body, count);
      if (acc > 0) post({ type: "progress", bytes: acc }); // flush tail
    } catch (err) {
      if (stopped) return; // aborted by stop() — a clean teardown, not an error
      if (acc > 0) post({ type: "progress", bytes: acc });
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}

/** Read a response body to completion, feeding each chunk's byte count to
 *  `count`. Prefers a BYOB reader reusing ONE ArrayBuffer so the hot loop does
 *  no per-chunk allocation/GC (the read-side throughput ceiling); falls back to
 *  the default reader if the body isn't a byte stream. */
async function readBody(
  body: ReadableStream<Uint8Array>,
  count: (n: number) => void,
): Promise<void> {
  let byob: ReadableStreamBYOBReader | null = null;
  try {
    byob = body.getReader({ mode: "byob" });
  } catch {
    byob = null; // not a byte stream (shouldn't happen for fetch) — fall back
  }
  if (byob) {
    let buf = new ArrayBuffer(READ_BUF_BYTES);
    for (;;) {
      const { value, done } = await byob.read(new Uint8Array(buf));
      if (done) break;
      if (value && value.byteLength) count(value.byteLength);
      // read() DETACHED our buffer and handed the same backing store back
      // inside `value`; reuse it for the next read so nothing is allocated.
      buf = value!.buffer as ArrayBuffer;
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
