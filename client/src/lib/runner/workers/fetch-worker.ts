/* One HTTP transfer lane: downloads count what they read; uploads post and the server counts. */

import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
import {
  progressWindow,
  readBytes,
  REPORT_GAP_MS,
  type ProgressDelta,
} from "./progressWindow";
import { incompressibleBlock } from "./payload";
import { classifyUploadFailure } from "./progressFeed";
import type { FlowDirection, LaneFailure } from "../contract";
import { MAX_STREAMS, ROUTES } from "../paths";
import {
  HTTP_SCHEMES,
  fields,
  integer,
  oneOf,
  optional,
  requestCredentials,
  requestHeaders,
  requestUrl,
} from "./inbound";

/* An upload lane is stopped by terminating the worker, so it has no shutdown message. */
type InMsg =
  | {
      type: "start";
      dir: FlowDirection;
      url: string;
      streams?: number;
      credentials?: RequestCredentials;
      headers?: Record<string, string>;
    }
  | { type: "measure"; seq: number };
/* An upload's local byte/time pair is only a bounded presentation hint; the receiver feed stays authoritative. */
type OutMsg =
  | { type: "progress"; bytes: number; elapsedMs: number; seq: number }
  | { type: "alive"; bytes: number; elapsedMs: number }
  | ({ type: "error"; detail: string } & LaneFailure)
  | { type: "auth-required" };

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

const LOST: LaneFailure = { reason: "connection-lost", retry: true };

/** Admission refusals end a download lane; any other status reconnects it. */
export const downloadFailure = (status: number): LaneFailure =>
  status === 429 || status === 503
    ? { reason: "server-busy", retry: false }
    : LOST;

export function fetchInit(
  credentials: RequestCredentials,
  headers?: HeadersInit,
): RequestInit {
  return {
    cache: "no-store",
    credentials,
    headers,
    redirect: redirectForCredentials(credentials),
  };
}

/* The POST target is about accuracy: request/response turnaround sits in server elapsed time. */
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

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg) => ctx.postMessage(m);
let init: RequestInit = fetchInit("same-origin");
let measureSeq = 0;
let progress = progressWindow(0, REPORT_GAP_MS);

/** Parse exactly the messages the lane owner sends; anything else throws to the owner's error handler. */
export function parseInMsg(data: unknown): InMsg {
  const m = fields(data);
  if (oneOf(m.type, ["start", "measure"], "type") === "measure")
    return {
      type: "measure",
      seq: integer(m.seq, 0, Number.MAX_SAFE_INTEGER, "seq"),
    };
  const dir = oneOf(m.dir, ["down", "up"], "direction");
  return {
    type: "start",
    dir,
    url: requestUrl(m.url, HTTP_SCHEMES, [
      dir === "down" ? ROUTES.download : ROUTES.upload,
    ]),
    streams: optional(m.streams, (n) =>
      integer(n, 1, MAX_STREAMS, "stream count"),
    ),
    credentials: requestCredentials(m.credentials),
    headers: requestHeaders(m.headers),
  };
}

ctx.onmessage = (e: MessageEvent<unknown>) => {
  // Only the owning page reaches a dedicated worker, through a port whose messages carry no origin.
  if (e.origin !== "") return;
  const msg = parseInMsg(e.data);
  if (msg.type === "measure") {
    measureSeq = msg.seq;
    progress.reset();
    return;
  }
  init = fetchInit(msg.credentials ?? "same-origin", msg.headers);
  if (msg.dir === "up") {
    const memory = (navigator as { deviceMemory?: number }).deviceMemory;
    void upload(msg.url, uploadPoolBytes(msg.streams ?? 1, memory));
  } else {
    progress = progressWindow(performance.now(), REPORT_GAP_MS);
    measureSeq = 0;
    void download(msg.url);
  }
};

/** A failure after session expiry is a sign-in failure, not a transport one. */
async function failed(error: unknown): Promise<void> {
  if (
    init.credentials === "include" &&
    (await sessionAuthenticationRequired(self.location.origin))
  )
    post({ type: "auth-required" });
  else post({ type: "error", detail: String(error), ...LOST });
}

function postProgress(delta: ProgressDelta | null): void {
  if (delta) post({ type: "progress", ...delta, seq: measureSeq });
}

async function download(url: string): Promise<void> {
  // Re-fetch until the measured window ends, even when one response reaches Content-Length.
  for (;;) {
    try {
      const res = await fetch(url, init);
      if (authenticationRequired(res)) return post({ type: "auth-required" });
      if (!res.ok || !res.body)
        return post({
          type: "error",
          detail: `HTTP ${res.status}`,
          ...downloadFailure(res.status),
        });
      await readBytes(res.body, (n) =>
        postProgress(progress.add(n, performance.now())),
      );
      postProgress(progress.flush());
    } catch (err) {
      postProgress(progress.flush());
      // The main thread decides whether to restart this lane.
      return failed(err);
    }
  }
}

/* Repeat immutable Blob references so a large reservoir shares one copied source block. */
function buildPool(bytes: number): Blob {
  const block = new Blob([incompressibleBlock().subarray(0, bytes)]);
  const parts: BlobPart[] = [];
  for (let remaining = bytes; remaining > 0; remaining -= block.size)
    parts.push(remaining >= block.size ? block : block.slice(0, remaining));
  return new Blob(parts);
}

/* Keep posting until the worker is terminated or a failure returns control to the runner. */
async function upload(url: string, poolBytes: number): Promise<void> {
  let pool: Blob;
  try {
    pool = buildPool(poolBytes);
  } catch (err) {
    // An unhandled rejection would not reach the owner's worker error handler.
    return post({
      type: "error",
      detail: `upload pool: ${String(err)}`,
      ...LOST,
    });
  }
  let next = Math.min(MIN_POST_BYTES, poolBytes);
  let rateEwma = 0;
  for (;;) {
    const sent = next;
    const postStart = performance.now();
    try {
      const res = await fetch(url, {
        ...init,
        method: "POST",
        body: pool.slice(0, sent),
      });
      if (authenticationRequired(res)) return post({ type: "auth-required" });
      // An unread echo pins the keep-alive connection the next POST needs.
      await res.arrayBuffer().catch(() => undefined);
      if (!res.ok)
        return post({
          type: "error",
          detail: `HTTP ${res.status}`,
          ...classifyUploadFailure(
            res.status,
            res.headers.get("X-Graphite-Upload-Refusal"),
          ),
        });
      // This is not an observation: the server progress feed owns byte/time accounting.
      const elapsedMs = performance.now() - postStart;
      post({ type: "alive", bytes: sent, elapsedMs });
      ({ bytes: next, ewma: rateEwma } = nextUploadBytes(
        sent,
        elapsedMs,
        rateEwma,
        poolBytes,
      ));
    } catch (err) {
      return failed(err);
    }
  }
}
