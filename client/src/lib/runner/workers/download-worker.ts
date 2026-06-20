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
 *     OOM — XHR-for-download is a non-starter.
 *   • Upload = XHR: we need upload.onprogress to count bytes ACTUALLY sent, and
 *     fetch has no upload-progress events at all; fetch *streaming* upload
 *     additionally requires HTTP/2 (dead end on our cleartext h1.1 origin).
 * The worker message protocol is identical both ways, so RealBackend's pool
 * treats them uniformly. WebTransport (Stage 5) is the truly-symmetric path.
 *
 * Self-contained (no imports) so it bundles cleanly as a Vite module worker
 * and dodges verbatimModuleSyntax/isolatedModules concerns.
 * ============================================================ */

/** Main → worker. */
type InMsg = { type: "start"; url: string } | { type: "stop" };
/** Worker → main. */
type OutMsg =
  | { type: "progress"; bytes: number }
  | { type: "error"; recoverable: boolean; detail: string };

// Narrow `self` to the dedicated-worker scope so postMessage/onmessage type
// cleanly under the combined DOM + WebWorker libs.
const ctx = self as unknown as DedicatedWorkerGlobalScope;

/** Post a delta no more often than this (ms); flushed on stream end / stop. */
const POST_INTERVAL_MS = 50;

let abort: AbortController | null = null;
let stopped = false;

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    stopped = false;
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
    try {
      const res = await fetch(url, { signal: abort.signal, cache: "no-store" });
      if (!res.ok || !res.body) {
        post({ type: "error", recoverable: true, detail: `HTTP ${res.status}` });
        return;
      }
      const reader = res.body.getReader();
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        if (value) {
          acc += value.byteLength; // count only — the chunk is then dropped
          const now = performance.now();
          if (now - lastPost >= POST_INTERVAL_MS) {
            post({ type: "progress", bytes: acc });
            acc = 0;
            lastPost = now;
          }
        }
      }
      if (acc > 0) post({ type: "progress", bytes: acc }); // flush tail
    } catch (err) {
      if (stopped) return; // aborted by stop() — a clean teardown, not an error
      if (acc > 0) post({ type: "progress", bytes: acc });
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}
