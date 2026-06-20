/* ============================================================
 * The Graphite Meter — Upload generate-and-POST worker (Stage 3)
 * ============================================================
 *
 * One worker per parallel upload stream. It generates ONE fixed incompressible
 * buffer with the canonical xorshift64* generator (crates/rng → WASM) and POSTs
 * it in a loop over plain HTTP/1.1 via XMLHttpRequest, measuring bytes via
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
 * Memory is fixed: ONE buffer per worker, generated once and reused for every
 * POST. Nothing grows.
 * ============================================================ */

import init, { ScrambledCounterRng } from "../../wasm/rng/gm_rng.js";
import { setDebugLogging, debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../../debug";

/** `debug`/`id` drive verbose per-stream logging only. */
type InMsg = { type: "start"; url: string; debug?: boolean; id?: number } | { type: "stop" };
type OutMsg =
  | { type: "progress"; bytes: number }
  | { type: "error"; recoverable: boolean; detail: string };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg) => ctx.postMessage(m);

/** Bytes per POST. Fixed + reused: large enough to amortize request overhead and
 *  keep the connection busy across a few RTTs, small enough to stay lean. */
const UPLOAD_BUF_BYTES = 4 * 1024 * 1024; // 4 MiB
/** Batch progress no more often than this (ms). */
const POST_INTERVAL_MS = 50;

let stopped = false;
let xhr: XMLHttpRequest | null = null;
let ready: Promise<unknown> | null = null;
/** The single reused payload (generated once on first start). */
let payload: Uint8Array | null = null;

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
    dbgWinBytes = 0;
    dbgTotal = 0;
    dbgWinStart = performance.now();
    void run(msg.url);
  } else if (msg.type === "stop") {
    stopped = true;
    xhr?.abort();
  }
};

/** A fresh random (seed, inc) per lane; inc forced odd for a full-period counter. */
function randomU64(): bigint {
  const a = new Uint32Array(2);
  crypto.getRandomValues(a);
  return (BigInt(a[1]) << 32n) | BigInt(a[0]);
}

async function run(url: string): Promise<void> {
  try {
    ready ??= init();
    await ready;
  } catch (err) {
    post({ type: "error", recoverable: false, detail: `wasm init failed: ${String(err)}` });
    return;
  }
  if (stopped) return;

  if (!payload) {
    const rng = new ScrambledCounterRng(randomU64(), randomU64() | 1n);
    payload = new Uint8Array(UPLOAD_BUF_BYTES);
    rng.fill_bytes(payload); // one-time fill; the bytes are reused every POST
  }
  postLoop(url);
}

/** POST the payload once; on completion, loop to keep the lane saturated. */
function postLoop(url: string): void {
  if (stopped || !payload) return;
  const buf = payload;
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
    const tail = buf.byteLength - lastLoaded;
    if (tail > 0) acc += tail;
    flush(true);
    if (!stopped) postLoop(url); // next POST on the same keep-alive connection
  };
  x.onerror = () => {
    flush(true);
    if (!stopped) post({ type: "error", recoverable: true, detail: "xhr upload error" });
  };
  x.onabort = () => flush(true);
  // Send the exact-fit backing ArrayBuffer (the view spans the whole buffer) —
  // avoids the Uint8Array<ArrayBufferLike> vs BufferSource generic mismatch.
  x.send(buf.buffer as ArrayBuffer);
}
