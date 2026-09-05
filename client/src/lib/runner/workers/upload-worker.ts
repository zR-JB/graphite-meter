/* The server drains and counts the bytes; upload-progress-worker.ts relays the authoritative total. */

import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
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

/** Pool floor keeps adaptive sizing useful on constrained devices. */
const MIN_POOL_BYTES = 2 * 1024 * 1024;
/* Reservoir for a device that reports no memory. */
const UNKNOWN_DEVICE_POOL_BYTES = 128 * 1024 * 1024;
/* Upload reservoir, divided across the lanes and also the sizer's ceiling. */
const UPLOAD_TOTAL_POOL_BYTES = 256 * 1024 * 1024;
/** Wall time each POST aims to span. */
const TARGET_POST_MS = 500;
/** Smallest POST, below which per-request overhead dominates. */
const MIN_POST_BYTES = 128 * 1024;

/* The POST target is about ACCURACY: request/response turnaround sits in server elapsed time. */
export function nextUploadBytes(
  prevBytes: number,
  elapsedMs: number,
  prevEwma: number,
  maxBytes: number,
): { bytes: number; ewma: number } {
  if (elapsedMs <= 0) return { bytes: prevBytes, ewma: prevEwma };
  const observed = (prevBytes / elapsedMs) * 1000;
  const ewma = prevEwma === 0 ? observed : 0.3 * observed + 0.7 * prevEwma;
  const want = (ewma * TARGET_POST_MS) / 1000;
  const stepped = Math.min(prevBytes * 2, Math.max(prevBytes * 0.5, want));
  return {
    bytes: Math.floor(Math.min(maxBytes, Math.max(MIN_POST_BYTES, stepped))),
    ewma,
  };
}

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
let pool: Blob | null = null;
/** Per-lane pool size to build, device-bounded so a phone cannot OOM. Also the sizer's upper clamp. */
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
    nextBytes = Math.min(MIN_POST_BYTES, poolTargetBytes);
    rateEwma = 0;
    void run(msg.url);
  }
};

/* Build the reused pool by repeating one filled block up to poolTargetBytes. */
function buildPool(): void {
  if (pool?.size === poolTargetBytes) return;
  const block = incompressibleBlock();
  const parts: BlobPart[] = [];
  let remaining = poolTargetBytes;
  while (remaining > 0) {
    const take = Math.min(remaining, block.byteLength);
    parts.push(take === block.byteLength ? block : block.subarray(0, take));
    remaining -= take;
  }
  pool = new Blob(parts, { type: "application/octet-stream" });
}

/* Release the tiny JSON echo so the keep-alive connection serves the next POST: an unread body pins it and stalls. */
async function drainForKeepAlive(res: Response): Promise<void> {
  try {
    await res.arrayBuffer();
  } catch {
    /* the POST already completed */
  }
}

/* Keep posting until the worker is terminated or a failure returns control to the runner. */
async function run(url: string): Promise<void> {
  try {
    buildPool();
  } catch (err) {
    // Report allocation failure explicitly; an unhandled rejection does not reach the owner’s worker error handler.
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
        body: pool.slice(0, sentBytes),
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
      ({ bytes: nextBytes, ewma: rateEwma } = nextUploadBytes(
        sentBytes,
        elapsedMs,
        rateEwma,
        poolTargetBytes,
      ));
    } catch (err) {
      // Check whether the failed POST followed session expiry before classifying it as a transport error.
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
