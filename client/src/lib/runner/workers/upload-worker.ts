/* The server drains and counts the bytes; upload-progress-worker.ts relays the authoritative total. */

import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
import { nextTransferBytes, type SizerCfg } from "./autosize";
import { incompressibleBlock } from "./payload";
import { classifyUploadFailure } from "../uploadFailure";
import type { RecoveryCause } from "../contract";

/* The lane is stopped by terminating the worker, so there is no shutdown message. */
type InMsg = {
  type: "start";
  url: string;
  streams?: number;
  credentials?: RequestCredentials;
  headers?: Record<string, string>;
};
/* Its local byte/time pair is only a bounded presentation hint; /upload/progress remains the authoritative source. */
type OutMsg =
  | { type: "alive"; bytes: number; elapsedMs: number }
  | {
      type: "error";
      recoverable: boolean;
      detail: string;
      cause?: RecoveryCause;
    }
  | { type: "auth-required" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
let credentials: RequestCredentials = "same-origin";
let headers: Record<string, string> = {};
const post = (m: OutMsg) => ctx.postMessage(m);

/** Pool floor keeps the autosizer useful on constrained devices. */
const MIN_POOL_BYTES = 2 * 1024 * 1024;
/* Reservoir for a device that reports no memory. */
const UNKNOWN_DEVICE_POOL_BYTES = 128 * 1024 * 1024;
/* Upload reservoir, divided across the lanes and also the sizer's ceiling. */
const UPLOAD_TOTAL_POOL_BYTES = 256 * 1024 * 1024;
/** Wall time each POST aims to span. */
const TARGET_POST_MS = 500;
/** Smallest POST, below which per-request overhead dominates. */
const MIN_POST_BYTES = 128 * 1024;

/* How the POST body reaches fetch. */
type UploadBody = "blob" | "arrayBuffer";
const UPLOAD_BODY: UploadBody = "blob";

/* ---- Closed-loop POST sizing, per worker (see autosize.ts) ---- */
/* The POST target is about ACCURACY: the request/response turnaround sits inside the server's elapsed-time. */
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

/* Explicit client/protocol refusals are terminal; a generic server failure remains a same-id reconnect. */
export function recoverableStatus(status: number): boolean {
  return status === 0 || status === 408 || (status >= 500 && status !== 503);
}

/* The reused incompressible pool, built on first start. */
let pool: Blob | Uint8Array<ArrayBuffer> | null = null;
/** Byte length of the pool as actually built. */
let poolBytes = 0;
/** Per-lane pool size to build, device-bounded so a phone cannot OOM. Also the autosizer's upper clamp. */
let poolTargetBytes = UPLOAD_TOTAL_POOL_BYTES;
/* Bytes the NEXT POST sends, the closed-loop variable. */
let nextBytes = MIN_POST_BYTES;
/** This lane's smoothed throughput (bytes/sec); 0 until the first POST completes. */
let rateEwma = 0;

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    credentials = msg.credentials ?? "same-origin";
    headers = msg.headers ?? {};
    const deviceMemory = (navigator as unknown as { deviceMemory?: number })
      .deviceMemory;
    poolTargetBytes = uploadPoolBytes(msg.streams ?? 1, deviceMemory);
    sizer.maxBytes = poolTargetBytes; // the pool is the size ceiling
    nextBytes = Math.min(MIN_POST_BYTES, poolTargetBytes);
    rateEwma = 0;
    void run(msg.url);
  }
};

/* Build the reused pool by repeating one filled block up to poolTargetBytes. */
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

/* A Blob slice is a view fetch reads through; a byte view is copied per POST. */
function bodyFor(sentBytes: number): BodyInit {
  return pool instanceof Blob
    ? pool.slice(0, sentBytes)
    : pool!.subarray(0, sentBytes);
}

/* Release the tiny JSON echo so the keep-alive connection serves the next POST: an unread body pins it and stalls. */
async function drainForKeepAlive(res: Response): Promise<void> {
  try {
    await res.arrayBuffer();
  } catch {
    /* the POST already completed */
  }
}

/* Mirrors download-worker.ts's re-fetch loop, and a network error ends the lane (RealBackend restarts it). */
async function run(url: string): Promise<void> {
  try {
    buildPool();
  } catch (err) {
// Left to reject, the promise takes no worker `error` event with it — unhandled rejections do not reach.
    post({
      type: "error",
      recoverable: true,
      detail: `upload pool: ${String(err)}`,
    });
    return;
  }
  if (!pool) {
    post({
      type: "error",
      recoverable: true,
      detail: "upload pool unavailable",
    });
    return;
  }

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
        const recoverable = recoverableStatus(res.status);
        post({
          type: "error",
          recoverable,
          detail: `HTTP ${res.status}`,
          cause: recoverable
            ? undefined
            : classifyUploadFailure(
                res.status,
                res.headers.get("X-Graphite-Upload-Refusal"),
              ),
        });
        return; // RealBackend decides whether to restart this lane
      }
// This is not an observation: the server progress feed owns byte/time accounting.
      const elapsedMs = performance.now() - postStart;
      post({ type: "alive", bytes: sentBytes, elapsedMs });
      ({ bytes: nextBytes, ewma: rateEwma } = nextTransferBytes(
        sentBytes,
        elapsedMs,
        rateEwma,
        sizer,
      ));
    } catch (err) {
// A POST that failed on an expired session is an auth failure, not a transport one, so the session is.
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
