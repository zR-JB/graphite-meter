/* ============================================================
 * The Graphite Meter: upload generate-and-POST worker
 * ============================================================
 * One worker per parallel upload stream. It builds one incompressible Blob pool
 * from CSPRNG bytes, which gzip and br cannot shrink, and POSTs zero-copy slices
 * of it in a loop over whichever HTTP version the origin negotiated. The server
 * drains and counts the bytes; upload-progress-worker.ts relays the
 * authoritative total. This lane saturates.
 * ============================================================ */

import {
  setDebugLogging,
  debugEnabled,
  dlog,
  fmtBytes,
  DebugWindow,
} from "../../debug";
import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
import { nextTransferBytes, type SizerCfg } from "./autosize";
import { incompressibleBlock } from "./payload";

/** `debug`/`id` drive verbose per-stream logging only. The lane is stopped by
 *  terminating the worker, so there is no shutdown message. */
type InMsg = {
  type: "start";
  url: string;
  debug?: boolean;
  id?: number;
  streams?: number;
  credentials?: RequestCredentials;
  headers?: Record<string, string>;
};
/** `alive` marks one POST the server drained, proving the lane is live. It
 *  carries no byte count: fetch has no upload-progress events, and the
 *  /upload/progress stream is the authoritative source. `error` restarts a lane. */
type OutMsg =
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "auth-required" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
let credentials: RequestCredentials = "same-origin";
let headers: Record<string, string> = {};
const post = (m: OutMsg) => ctx.postMessage(m);

/** Pool floor keeps the autosizer useful on constrained devices. */
const MIN_POOL_BYTES = 2 * 1024 * 1024;
/** Reservoir for a device that reports no memory. An absent value is not
 *  evidence of a large device: Chromium reports navigator.deviceMemory, so this
 *  is the Firefox/Safari tier, phones included. */
const UNKNOWN_DEVICE_POOL_BYTES = 128 * 1024 * 1024;
/** Upload reservoir, divided across the lanes and also the sizer's ceiling.
 *  Worth +10.9% at 256 MiB over 64 MiB; see docs/BENCHMARKS.md. */
const UPLOAD_TOTAL_POOL_BYTES = 256 * 1024 * 1024;
/** Wall time each POST aims to span. */
const TARGET_POST_MS = 500;
/** Smallest POST, below which per-request overhead dominates. */
const MIN_POST_BYTES = 128 * 1024;

/** How the POST body reaches fetch. A Blob slice is a view fetch reads through;
 *  an ArrayBuffer is copied per POST, which is the cost the Blob path avoids. */
type UploadBody = "blob" | "arrayBuffer";
const UPLOAD_BODY: UploadBody = "blob";

/* ---- Closed-loop POST sizing, per worker (see autosize.ts) ---- */
/** The POST target is about ACCURACY: the request/response turnaround sits
 *  inside the server's elapsed-time denominator, so a too-short POST lowers the
 *  measured rate. Interleaved lanes cover each other's turnaround.
 *  maxBytes is the pool size, set on `start`. */
const sizer: SizerCfg = {
  targetMs: TARGET_POST_MS,
  minBytes: MIN_POST_BYTES,
  maxBytes: MIN_POST_BYTES,
  alpha: 0.3,
  stepUp: 2,
  stepDown: 0.5,
};

/** Divide the device-scaled total reservoir across the actual lane count. */
export function uploadPoolBytes(
  streams: number,
  deviceMemory?: number,
  totalPoolBytes = UPLOAD_TOTAL_POOL_BYTES,
): number {
  streams = Math.max(1, streams);
  const reservoir =
    typeof deviceMemory !== "number"
      ? Math.min(UNKNOWN_DEVICE_POOL_BYTES, totalPoolBytes)
      : deviceMemory <= 2
        ? 16 * 1024 * 1024
        : deviceMemory <= 4
          ? 24 * 1024 * 1024
          : totalPoolBytes;
  return Math.max(MIN_POOL_BYTES, Math.floor(reservoir / streams));
}

/** Whether retrying the lane after a non-OK POST is worthwhile. 429 (rate
 *  limited), 413 (too large), 503 (unavailable) and 410 (gone) are terminal for
 *  this run: re-POSTing hammers a server that will not take it. Everything else,
 *  including 500 and any network or abort error, counts as transient. */
export function recoverableStatus(status: number): boolean {
  return !(
    status === 429 ||
    status === 413 ||
    status === 503 ||
    status === 410
  );
}

/** The reused incompressible pool, built on first start. A Blob slice is a view
 *  fetch reads through, which is why Blob is the default. */
let pool: Blob | Uint8Array<ArrayBuffer> | null = null;
/** Byte length of the pool as actually built. */
let poolBytes = 0;
/** Per-lane pool size to build, device-bounded so a phone cannot OOM. Also the
 *  autosizer's upper clamp. */
let poolTargetBytes = UPLOAD_TOTAL_POOL_BYTES;
/** Bytes the NEXT POST sends, the closed-loop variable. Starts at the minimum
 *  for a fast first sample, then tracks the target times this lane's rate. */
let nextBytes = MIN_POST_BYTES;
/** This lane's smoothed throughput (bytes/sec); 0 until the first POST completes. */
let rateEwma = 0;

/** Stream index, tagging debug lines only (`ul-worker#<id>`). */
let streamId = 0;
/** Completed-POST debug window: one step per POST rather than byte-granular,
 *  enough to show whether turnaround leaves the wire idle. */
const dbg = new DebugWindow();

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    setDebugLogging(msg.debug ?? false);
    streamId = msg.id ?? 0;
    credentials = msg.credentials ?? "same-origin";
    headers = msg.headers ?? {};
    const deviceMemory = (navigator as unknown as { deviceMemory?: number })
      .deviceMemory;
    poolTargetBytes = uploadPoolBytes(msg.streams ?? 1, deviceMemory);
    sizer.maxBytes = poolTargetBytes; // the pool is the size ceiling
    nextBytes = Math.min(MIN_POST_BYTES, poolTargetBytes);
    rateEwma = 0;
    dbg.reset();
    void run(msg.url);
  }
};

/** Build the reused pool by repeating one filled block up to poolTargetBytes.
 *  The Blob copies each part into its own backing store, so the construction
 *  heap peaks at ~block + pool. Every POST then slices a view of it. */
function buildPool(): void {
  const wantBlob = UPLOAD_BODY === "blob";
  if (
    pool &&
    poolBytes === poolTargetBytes &&
    pool instanceof Blob === wantBlob
  )
    return;
  const block = incompressibleBlock();
  if (wantBlob) {
    const parts: BlobPart[] = [];
    let remaining = poolTargetBytes;
    while (remaining > 0) {
      const take = Math.min(remaining, block.byteLength);
      parts.push(take === block.byteLength ? block : block.subarray(0, take));
      remaining -= take;
    }
    pool = new Blob(parts, { type: "application/octet-stream" });
  } else {
    const bytes = new Uint8Array(new ArrayBuffer(poolTargetBytes));
    for (let off = 0; off < bytes.length; off += block.byteLength)
      bytes.set(
        block.subarray(0, Math.min(block.byteLength, bytes.length - off)),
        off,
      );
    pool = bytes;
  }
  poolBytes = poolTargetBytes;
}

/** A Blob slice is a view fetch reads through; a byte view is copied per POST.
 *  That copy is the cost the Blob path exists to avoid. */
function bodyFor(sentBytes: number): BodyInit {
  return pool instanceof Blob
    ? pool.slice(0, sentBytes)
    : pool!.subarray(0, sentBytes);
}

/** Release the tiny JSON echo so the keep-alive connection serves the next POST:
 *  an unread body pins it and stalls the lane. The POST is already complete, so
 *  a failed release costs at most one connection. */
async function drainForKeepAlive(res: Response): Promise<void> {
  try {
    await res.arrayBuffer();
  } catch {
    /* the POST already completed */
  }
}

/** Drive the lane for the whole stage: POSTs of adaptively-sized pool slices in
 *  a loop. Mirrors download-worker.ts's re-fetch loop, and a network error ends
 *  the lane (RealBackend restarts it). Each completed POST resizes the NEXT one. */
async function run(url: string): Promise<void> {
  buildPool();
  if (!pool) return;

  for (;;) {
    const sentBytes = nextBytes;
    const postStart = performance.now();
    try {
      const res = await fetch(url, {
        method: "POST",
        body: bodyFor(sentBytes),
        cache: "no-store",
        headers: { ...headers, "Content-Type": "application/octet-stream" },
        credentials,
        redirect: redirectForCredentials(credentials),
      });
      if (authenticationRequired(res)) {
        post({ type: "auth-required" });
        return;
      }
      await drainForKeepAlive(res);
      if (!res.ok) {
        post({
          type: "error",
          recoverable: recoverableStatus(res.status),
          detail: `HTTP ${res.status}`,
        });
        return; // RealBackend decides whether to restart this lane
      }
      // The server drained a full slice: the lane is alive. No bytes travel here,
      // /upload/progress is authoritative; this only resets the restart counter.
      post({ type: "alive" });
      ({ bytes: nextBytes, ewma: rateEwma } = nextTransferBytes(
        sentBytes,
        performance.now() - postStart,
        rateEwma,
        sizer,
      ));
      if (debugEnabled()) {
        const window = dbg.add(sentBytes);
        if (window)
          dlog(`ul-worker#${streamId}`, "post-complete", {
            rate: window.rate,
            postSize: fmtBytes(nextBytes),
            window: window.window,
            total: window.total,
            dt: window.dt,
          });
      }
    } catch (err) {
      // A POST that failed on an expired session is an auth failure, not a
      // transport one, so the session is re-checked before the error is reported.
      if (
        credentials === "include" &&
        (await sessionAuthenticationRequired(self.location.origin))
      ) {
        post({ type: "auth-required" });
        return;
      }
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}
