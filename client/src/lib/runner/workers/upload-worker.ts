/* ============================================================
 * The Graphite Meter — Upload generate-and-POST worker (Stage 3)
 * ============================================================
 *
 * One worker per parallel upload stream. It generates ONE fixed incompressible
 * Blob with `crypto.getRandomValues` (no WASM — filled once and reused for every
 * POST, so the RNG is never on the hot path; CSPRNG bytes are indistinguishable
 * from xorshift64* to gzip/br, so incompressibility holds) and POSTs it in a loop
 * over plain HTTP/1.1 via `fetch`. The SERVER drains + counts the bytes and
 * relays the authoritative count over /ws/upload (see upload-progress-worker.ts);
 * this worker just keeps the lane saturated and is otherwise measurement-blind.
 *
 * ── Why a fixed-Blob `fetch` (and NOT a streaming fetch) ──
 * A `fetch` whose body is a *ReadableStream* (`duplex:'half'`) is the streaming
 * upload primitive — and it requires HTTP/2 in Chrome (→ ALPN failure on our
 * cleartext h1.1 origin) and is unimplemented in Firefox. That path is NEVER used
 * here. A `fetch` whose body is a *fixed Blob* has a known Content-Length and is
 * an ordinary h1.1 request that works in every target browser — exactly like the
 * XHR repeated-Blob POST it replaces, but with the cleaner fetch/AbortController
 * ergonomics the download worker already uses (same abort + re-loop shape) and a
 * real `res.ok`/`res.status` so a 4xx/5xx is handled instead of blindly re-POSTed.
 *
 * ── Why no progress events (server-authoritative) ──
 * `fetch` has no upload-progress events at all, and we no longer want them: the
 * upload figure is the SERVER's drained byte count (the only count downstream of
 * every browser/proxy send buffer — it can lag the wire but never lead it). So
 * this worker reports only lane liveness: one `{type:'alive'}` per completed POST
 * (proving the lane recovered, for the restart logic) and `{type:'error'}` on a
 * failed POST. It NEVER reports bytes — the /ws/upload count is the sole source.
 * (The old `upload.onprogress` byte counting is gone, along with its watchdog
 * keepalive — the 100 ms server frames are the heartbeat now; a dropped progress
 * socket freezes measured-time via the progress worker's stall/resume.)
 *
 * ── Why fetch here mirrors download-worker.ts ──
 * Download = fetch + body.getReader(): read-and-DISCARD a streamed RESPONSE at
 * O(1) memory. Upload = fetch + fixed Blob body: stream a generated REQUEST from
 * one Blob and read nothing back (a tiny JSON echo, drained to free the keep-alive
 * connection). Same fetch/abort/re-loop skeleton both directions; the platform no
 * longer forces XHR on the upload side now that we don't need onprogress.
 *
 * Message protocol (RealBackend's pool drives both directions):
 *   in:  { type: 'start', url, debug?, id?, streams? } | { type: 'stop' }
 *   out: { type: 'alive' } | { type: 'error', recoverable, detail }
 *
 * ── Why a Blob and not an ArrayBuffer (the memory-blowup fix, unchanged) ──
 * `fetch(body: arrayBuffer)` (like `xhr.send(arrayBuffer)`) COPIES the body bytes
 * into the request every call, so looping POSTs of a multi-MiB ArrayBuffer churn a
 * fresh copy per request — on a fast (loopback) link that created copies at
 * gigabytes/sec, far faster than GC reclaimed them, ballooning the heap to many GB.
 * `fetch(body: blob)` REFERENCES the Blob instead: one Blob, generated once,
 * streamed from for every POST → no per-request JS-heap copy (the engine serialises
 * the Blob's backing store straight to the socket). The footprint stays flat.
 * NEVER revert to a per-POST ArrayBuffer/slice. See docs/THROUGHPUT_MEASUREMENT.md.
 * ============================================================ */

import { setDebugLogging, debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../../debug";

/** `debug`/`id` drive verbose per-stream logging only. `streams` is the active
 *  parallel-stream count, used to split UPLOAD_TOTAL_BUF_BYTES per worker. */
type InMsg =
  | { type: "start"; url: string; debug?: boolean; id?: number; streams?: number }
  | { type: "stop" };
/** `alive` = one POST drained by the server (lane is live; NO byte count — the
 *  /ws/upload socket carries the authoritative count). `error` drives lane restart. */
type OutMsg =
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg) => ctx.postMessage(m);

/** TOTAL upload payload budget across ALL streams — the single knob to tune.
 *  Each worker uses TOTAL / parallelStreams (see `bufBytes`), so the combined
 *  payload reservoir stays CONSTANT no matter how many streams run. Sized LARGE on
 *  purpose so each POST lasts ~100 ms+ and the request→response turnaround between
 *  POSTs is amortised to a few % (a small body drains in a few ms on a fast link,
 *  so that idle gap would cap upload far below download). The server counts the
 *  bytes byte-granular as it drains them, so a slow link still reports its real
 *  partial throughput over /ws/upload — independent of how big each POST is.
 *  Cost: ONE Blob of (TOTAL / streams) per worker, allocated once and reused;
 *  fetch references it, so the size never grows memory. */
const UPLOAD_TOTAL_BUF_BYTES = 64 * 1024 * 1024; // 64 MiB desktop default (÷ streams per worker)
/** Per-worker payload floor. Below this a POST drains in a few ms on a fast link,
 *  so the request→response turnaround dominates — keep each POST ~100 ms+. */
const MIN_PER_WORKER_BYTES = 2 * 1024 * 1024;
/** The payload is built by repeating ONE filled block, so the peak transient heap
 *  during construction is ~block + payload, not the 2× a fresh Uint8Array(bufBytes)
 *  plus its Blob copy would cost (the iOS-Safari tab-kill guard at 1 stream). */
const FILL_BLOCK_BYTES = 4 * 1024 * 1024;
/** crypto.getRandomValues' hard per-call byte quota. */
const RNG_CHUNK_BYTES = 65536;

/** Total upload reservoir scaled to device memory so a low-RAM phone doesn't OOM
 *  building the Blob. `navigator.deviceMemory` is a GiB hint (undefined on Firefox/
 *  Safari → treated as desktop). */
function uploadTotalBudget(): number {
  const dm = (navigator as unknown as { deviceMemory?: number }).deviceMemory;
  if (typeof dm === "number") {
    if (dm <= 2) return 16 * 1024 * 1024;
    if (dm <= 4) return 24 * 1024 * 1024;
  }
  return UPLOAD_TOTAL_BUF_BYTES;
}

/** Map a non-OK POST status to whether retrying the lane is worthwhile.
 *  429 (rate-limited) / 413 (too large) / 503 (unavailable) / 410 (gone) are
 *  terminal for this run — re-POSTing just hammers a server that won't take it.
 *  Everything else (incl. 500 and any network/abort error) is treated transient. */
function recoverableStatus(status: number): boolean {
  return !(status === 429 || status === 413 || status === 503 || status === 410);
}

let stopped = false;
/** Aborts the in-flight POST on `stop` (mirrors download-worker.ts). */
let abort: AbortController | null = null;
/** The single reused payload (generated once on first start). A Blob — NOT an
 *  ArrayBuffer — so each fetch references it instead of copying it. */
let payload: Blob | null = null;
/** The 4 MiB incompressible source block, filled once with CSPRNG bytes and
 *  repeated to build each payload (caps the construction-time heap peak). Typed
 *  over ArrayBuffer (not the default ArrayBufferLike) so it is a valid BlobPart. */
let fillBlock: Uint8Array<ArrayBuffer> | null = null;
/** The payload's byte length, cached so the debug log can tally completed POSTs. */
let payloadBytes = 0;
/** Per-worker payload size = uploadTotalBudget() / parallelStreams (≥ floor),
 *  fixed on `start` so the combined reservoir is independent of stream count (and
 *  device-bounded so a phone can't OOM). */
let bufBytes = UPLOAD_TOTAL_BUF_BYTES;

/** Stream index, only used to tag debug lines (`ul-worker#<id>`). */
let streamId = 0;
/** Completed-POST debug window: bytes fully POSTed (server-drained) since the last
 *  1 Hz log + its start time + the running per-stream total. Coarser than the old
 *  onprogress raw-send window (one step per POST, not byte-granular), but it still
 *  shows whether the request→response turnaround is leaving the wire idle. */
let dbgWinBytes = 0;
let dbgWinStart = 0;
let dbgTotal = 0;

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    stopped = false;
    setDebugLogging(msg.debug ?? false);
    streamId = msg.id ?? 0;
    // Split the device-scaled reservoir across the active streams, with a floor.
    bufBytes = Math.max(
      MIN_PER_WORKER_BYTES,
      Math.floor(uploadTotalBudget() / Math.max(1, msg.streams ?? 1)),
    );
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
    crypto.getRandomValues(b.subarray(off, Math.min(off + RNG_CHUNK_BYTES, b.length)));
  }
  fillBlock = b;
  return b;
}

/** Build the reused payload by REPEATING one filled block up to bufBytes. The Blob
 *  copies each part into its own backing store, so the construction-time heap peak
 *  is ~block + payload (not the 2× a fresh Uint8Array(bufBytes) + its Blob copy
 *  would cost). fetch then streams from it without a per-POST copy. */
function buildPayload(): void {
  if (payload && payloadBytes === bufBytes) return;
  const block = incompressibleBlock();
  const parts: BlobPart[] = [];
  let remaining = bufBytes;
  while (remaining > 0) {
    const take = Math.min(remaining, block.byteLength);
    parts.push(take === block.byteLength ? block : block.subarray(0, take));
    remaining -= take;
  }
  payload = new Blob(parts, { type: "application/octet-stream" });
  payloadBytes = bufBytes;
}

/** POST the fixed Blob in a loop to keep the lane saturated for the whole stage.
 *  Mirrors download-worker.ts's re-fetch loop: a fresh AbortController per POST,
 *  `stop` aborts it, a network error ends the lane (RealBackend restarts it). */
async function run(url: string): Promise<void> {
  if (stopped) return;
  buildPayload();
  if (!payload) return;
  const body = payload;

  while (!stopped) {
    abort = new AbortController();
    try {
      const res = await fetch(url, {
        method: "POST",
        body,
        signal: abort.signal,
        cache: "no-store",
        headers: { "Content-Type": "application/octet-stream" },
      });
      // Drain the tiny JSON echo so this keep-alive connection is reusable for the
      // next POST (an unread body can pin the connection and stall the lane).
      await res.arrayBuffer().catch(() => {});
      if (!res.ok) {
        post({ type: "error", recoverable: recoverableStatus(res.status), detail: `HTTP ${res.status}` });
        return; // RealBackend decides whether to restart this lane
      }
      // One full payload was drained by the server: the lane is alive. NO bytes —
      // the /ws/upload count is authoritative; this only resets the restart counter.
      post({ type: "alive" });
      if (debugEnabled()) {
        dbgWinBytes += payloadBytes;
        dbgTotal += payloadBytes;
        const now = performance.now();
        const dt = now - dbgWinStart;
        if (dt >= 1000) {
          dlog(`ul-worker#${streamId}`, "post-complete", {
            rate: fmtRate(dbgWinBytes / (dt / 1000)),
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
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}
