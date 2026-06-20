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
 * the wire/server). `xhr.send(blob)` REFERENCES the Blob instead of copying it:
 * one Blob, generated once, streamed from for every POST → zero per-request
 * allocation. The footprint then stays flat and `onprogress` tracks the real
 * socket throughput (so the app matches the wire/server figure).
 * See docs/THROUGHPUT_MEASUREMENT.md for the full write-up.
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

/** Bytes per POST. Fixed + reused (one buffer per worker, generated once).
 *  Sized LARGE on purpose: each POST is a discrete request, and the connection
 *  goes idle during the request→response turnaround between POSTs. A small body
 *  (the old 4 MiB) drains in a few ms on a fast link, so that turnaround gap was
 *  a big fraction of wall-time and capped upload far below download. A big body
 *  makes each POST last ~100 ms+, so the turnaround is amortised to a few %.
 *  Accuracy is unaffected: upload.onprogress counts bytes byte-granular AS they
 *  flush to the socket (not per-POST), so even a slow link that never finishes
 *  one POST in a stage still reports its real partial throughput.
 *  Cost: ONE Blob of this size per worker (×parallelStreams), allocated once —
 *  the send no longer copies it, so this can be large without growing memory. */
const UPLOAD_BUF_BYTES = 16 * 1024 * 1024; // 16 MiB
/** Batch progress no more often than this (ms). */
const POST_INTERVAL_MS = 50;

let stopped = false;
let xhr: XMLHttpRequest | null = null;
let ready: Promise<unknown> | null = null;
/** The single reused payload (generated once on first start). A Blob — NOT an
 *  ArrayBuffer — so each xhr.send() references it instead of copying it. */
let payload: Blob | null = null;
/** The payload's byte length, cached so onload can tally the tail without
 *  touching the Blob. */
let payloadBytes = 0;

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
    const bytes = new Uint8Array(UPLOAD_BUF_BYTES);
    rng.fill_bytes(bytes); // one-time fill; the bytes are reused every POST
    // Wrap once in a Blob. send(Blob) streams from this without copying, so the
    // POST loop allocates nothing per request (the memory-blowup fix).
    payload = new Blob([bytes], { type: "application/octet-stream" });
    payloadBytes = bytes.byteLength;
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
