/* ============================================================
 * The Graphite Meter — Upload generate-and-stream worker (Stage 3)
 * ============================================================
 *
 * One worker per parallel upload stream. It generates incompressible bytes with
 * the canonical xorshift64* generator compiled to WASM (crates/rng) and streams
 * them as a chunked POST body to /upload; the server drains + counts them. The
 * payload is produced in the worker and handed straight to the network, so byte
 * generation never competes with gauge rendering on the main thread — only tiny
 * batched `{ bytes }` deltas (bytes handed to the stream) come back.
 *
 * Message protocol is identical to the download worker, so the RealBackend
 * worker-pool harness drives both directions the same way:
 *   in:  { type: 'start', url } | { type: 'stop' }
 *   out: { type: 'progress', bytes } | { type: 'error', recoverable, detail }
 *
 * The lane streams continuously until `stop` aborts the fetch (the measured
 * window owns the duration, not a fixed body size). Mirrors the proven
 * reference-demos/upload-demo worker.
 * ============================================================ */

import init, { ScrambledCounterRng } from "../../wasm/rng/gm_rng.js";

type InMsg = { type: "start"; url: string } | { type: "stop" };
type OutMsg =
  | { type: "progress"; bytes: number }
  | { type: "error"; recoverable: boolean; detail: string };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg) => ctx.postMessage(m);

/** 64 KiB chunks — smooth streaming without oversized per-pull allocations. */
const CHUNK_BYTES = 64 * 1024;
/** Batch progress no more often than this (ms). */
const POST_INTERVAL_MS = 50;

let abort: AbortController | null = null;
let stopped = false;
/** Resolves once the WASM module is initialised (once per worker). */
let ready: Promise<unknown> | null = null;

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

/** A fresh random (seed, inc) per lane so each stream is a distinct
 *  incompressible sequence. inc is forced odd for a full-period counter. */
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

  const rng = new ScrambledCounterRng(randomU64(), randomU64() | 1n);
  abort = new AbortController();

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

  // Backpressure-driven byte source: the fetch pulls a chunk only when the
  // network is ready for more, so `acc` tracks bytes actually handed to the
  // socket. Never closes on its own — `stop` aborts it.
  const body = new ReadableStream<Uint8Array>({
    pull(controller) {
      if (stopped) {
        controller.close();
        return;
      }
      const chunk = new Uint8Array(CHUNK_BYTES);
      rng.fill_bytes(chunk);
      controller.enqueue(chunk);
      acc += chunk.byteLength;
      flush(false);
    },
    cancel() {
      stopped = true;
    },
  });

  try {
    const res = await fetch(url, {
      method: "POST",
      body,
      signal: abort.signal,
      headers: { "Content-Type": "application/octet-stream" },
      // Required for a streaming (ReadableStream) request body.
      duplex: "half",
    } as RequestInit & { duplex: "half" });
    flush(true);
    if (!res.ok) {
      post({ type: "error", recoverable: true, detail: `HTTP ${res.status}` });
    }
  } catch (err) {
    flush(true);
    if (stopped) return; // aborted by stop() — clean teardown, not an error
    post({ type: "error", recoverable: true, detail: String(err) });
  }
}
