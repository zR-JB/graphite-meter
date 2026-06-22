/* ============================================================
 * The Graphite Meter — Upload generate-and-POST worker (Stage 3)
 * ============================================================
 *
 * One worker per parallel upload stream. It generates ONE fixed incompressible
 * buffer with `crypto.getRandomValues` (no WASM — the buffer is filled once and
 * reused cyclically, so the RNG is never on the hot path; CSPRNG bytes are
 * indistinguishable from xorshift64* to gzip/br, so incompressibility holds) and
 * POSTs it in a loop over plain HTTP/1.1 via XMLHttpRequest, measuring bytes via
 * `upload.onprogress` (so we count bytes ACTUALLY sent, never generated-but-
 * unsent). The server drains + counts them.
 *
 * Why XHR and not a streaming `fetch` body: a `fetch` with a ReadableStream
 * request body (`duplex:'half'`) requires HTTP/2 in Chrome (→ ALPN failure on
 * our cleartext h1.1 origin) and is unsupported in Firefox. XHR upload works
 * over HTTP/1.1 in every browser — and each worker is its own TCP connection,
 * exactly the multi-TCP model (ARCHITECTURE §1). The WebTransport upload path
 * (the demo's `upload-worker.js`) arrives in Stage 5.
 *
 * (Conversely, download-worker.ts uses fetch, NOT XHR: it must read-and-discard
 * a streamed response at O(1) memory, which XHR can't do — it buffers the whole
 * response. See that file's header for the full fetch-vs-XHR rationale.)
 *
 * Message protocol matches the download worker so the RealBackend pool drives
 * both directions identically:
 *   in:  { type: 'start', url } | { type: 'stop' }
 *   out: { type: 'progress', bytes } | { type: 'error', recoverable, detail }
 *
 * Memory is fixed: ONE Blob per worker, generated once and reused for every
 * POST. Nothing grows.
 *
 * ── Why a Blob and not an ArrayBuffer (the memory-blowup fix) ──
 * `xhr.send(arrayBuffer)` COPIES the body bytes into the request every call, so
 * looping POSTs of a 64/32/4 MiB ArrayBuffer churns a fresh multi-MiB copy per
 * request. On a fast (loopback) link each POST completes in a few ms, so we
 * created copies at gigabytes/sec — far faster than GC reclaims them. The heap
 * ballooned to many GB, GC pauses froze the UI, and `upload.onprogress` counted
 * those buffered-but-undelivered bytes as "sent" (so the app over-reported vs
 * the wire/server). `xhr.send(blob)` REFERENCES the Blob instead of copying it
 * into the request: one Blob, generated once, streamed from for every POST → no
 * per-request JS-heap copy (the engine serialises the Blob's backing store
 * straight to the socket). The footprint then stays flat and `onprogress` tracks
 * the real socket throughput (so the app matches the wire/server figure).
 * See docs/THROUGHPUT_MEASUREMENT.md for the full write-up.
 * ============================================================ */

import { setDebugLogging, debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../../debug";

/** `debug`/`id` drive verbose per-stream logging only. `streams` is the active
 *  parallel-stream count, used to split UPLOAD_TOTAL_BUF_BYTES per worker. */
type InMsg =
  | { type: "start"; url: string; debug?: boolean; id?: number; streams?: number }
  | { type: "stop" };
type OutMsg =
  | { type: "progress"; bytes: number }
  | { type: "error"; recoverable: boolean; detail: string };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg) => ctx.postMessage(m);

/** TOTAL upload payload budget across ALL streams — the single knob to tune.
 *  Each worker uses TOTAL / parallelStreams (see `bufBytes`), so the combined
 *  in-flight reservoir (the bytes upload.onprogress can count ahead of what the
 *  server has actually drained) stays CONSTANT no matter how many streams run.
 *  That's the point: bump streams for more parallelism WITHOUT silently
 *  enlarging the reservoir — which is what made "6 streams" drift exactly like a
 *  too-big per-worker body did (the per-stream body, not the total, had been the
 *  constant). Sized LARGE on purpose so each POST lasts ~100 ms+ and the
 *  request→response turnaround between POSTs is amortised to a few % (a small
 *  body drains in a few ms on a fast link, so that idle gap capped upload far
 *  below download). upload.onprogress still counts bytes byte-granular AS they
 *  flush to the socket (not per-POST), so a slow link reports its real partial
 *  throughput; honesty of that LIVE figure relies on the reservoir staying
 *  bounded — see docs/THROUGHPUT_MEASUREMENT.md.
 *  Cost: ONE Blob of (TOTAL / streams) per worker, allocated once and reused;
 *  send() references it, so the size never grows memory. */
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
/** Batch progress no more often than this (ms). */
const POST_INTERVAL_MS = 50;

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

let stopped = false;
let xhr: XMLHttpRequest | null = null;
/** The single reused payload (generated once on first start). A Blob — NOT an
 *  ArrayBuffer — so each xhr.send() references it instead of copying it. */
let payload: Blob | null = null;
/** The 4 MiB incompressible source block, filled once with CSPRNG bytes and
 *  repeated to build each payload (caps the construction-time heap peak). Typed
 *  over ArrayBuffer (not the default ArrayBufferLike) so it is a valid BlobPart. */
let fillBlock: Uint8Array<ArrayBuffer> | null = null;
/** The payload's byte length, cached so onload can tally the tail without
 *  touching the Blob. */
let payloadBytes = 0;
/** Per-worker payload size = uploadTotalBudget() / parallelStreams (≥ floor),
 *  fixed on `start` so the combined in-flight reservoir is independent of stream
 *  count (and device-bounded so a phone can't OOM). */
let bufBytes = UPLOAD_TOTAL_BUF_BYTES;

/** Stream index, only used to tag debug lines (`ul-worker#<id>`). */
let streamId = 0;
/** Raw-send debug window: bytes flushed to the socket (via upload.onprogress)
 *  since the last 1 Hz log + its start time + the running per-stream total.
 *  This is the real sent count, spanning POST boundaries, so it shows whether
 *  the request/response turnaround is leaving the wire idle. */
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
    run(msg.url);
  } else if (msg.type === "stop") {
    stopped = true;
    xhr?.abort();
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

function run(url: string): void {
  if (stopped) return;

  if (!payload || payloadBytes !== bufBytes) {
    // Build the payload by REPEATING one filled block up to bufBytes. The Blob
    // copies each part into its own backing store, so the construction-time heap
    // peak is ~block + payload (not the 2× a fresh Uint8Array(bufBytes) + its Blob
    // copy would cost). send(Blob) then streams from it without a per-POST copy.
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
  postLoop(url);
}

/** POST the payload once; on completion, loop to keep the lane saturated. */
function postLoop(url: string): void {
  if (stopped || !payload) return;
  const body = payload;
  const x = new XMLHttpRequest();
  xhr = x;

  let lastLoaded = 0;
  let acc = 0;
  let lastPost = performance.now();
  const flush = (force: boolean) => {
    const now = performance.now();
    if (acc > 0 && (force || now - lastPost >= POST_INTERVAL_MS)) {
      post({ type: "progress", bytes: acc });
      acc = 0;
      lastPost = now;
    }
  };

  x.open("POST", url);
  x.setRequestHeader("Content-Type", "application/octet-stream");
  // upload.onprogress reports bytes flushed to the socket — the real sent count.
  x.upload.onprogress = (e: ProgressEvent) => {
    const d = e.loaded - lastLoaded;
    lastLoaded = e.loaded;
    if (d > 0) {
      acc += d;
      flush(false);
      // Verbose: raw bytes flushed to the socket, 1 Hz, spanning POST
      // boundaries — the ground truth for the upload turnaround question.
      if (debugEnabled()) {
        dbgWinBytes += d;
        dbgTotal += d;
        const now = performance.now();
        const dt = now - dbgWinStart;
        if (dt >= 1000) {
          dlog(`ul-worker#${streamId}`, "raw-send", {
            rate: fmtRate(dbgWinBytes / (dt / 1000)),
            window: fmtBytes(dbgWinBytes),
            total: fmtBytes(dbgTotal),
            dt: fmtMs(dt),
          });
          dbgWinBytes = 0;
          dbgWinStart = now;
        }
      }
    }
  };
  x.onload = () => {
    // Count any tail progress events didn't report (fast POSTs may skip them).
    const tail = payloadBytes - lastLoaded;
    if (tail > 0) acc += tail;
    flush(true);
    if (!stopped) postLoop(url); // next POST on the same keep-alive connection
  };
  x.onerror = () => {
    flush(true);
    if (!stopped) post({ type: "error", recoverable: true, detail: "xhr upload error" });
  };
  x.onabort = () => flush(true);
  // Send the reused Blob: the browser STREAMS the body from it without copying,
  // so the POST loop allocates nothing per request. (send(ArrayBuffer) would
  // copy the bytes every call — the old memory blowup.)
  x.send(body);
}
