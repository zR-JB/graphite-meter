/* The Graphite Meter: download read-and-count worker ============================================================. */

import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
import { progressWindow, type ProgressDelta } from "./progressWindow";
import { READ_BUF_BYTES, REPORT_GAP_MS } from "./tuning";

/* Main → worker. */
type InMsg =
  | {
      type: "start";
      url: string;
      credentials?: RequestCredentials;
      headers?: HeadersInit;
    }
  | { type: "measure"; seq: number };
/** Worker → main. */
type OutMsg =
  | { type: "progress"; bytes: number; elapsedMs: number; seq: number }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "auth-required" };

export function recoverableDownloadStatus(status: number): boolean {
  return status !== 429 && status !== 503;
}

export function downloadFetchInit(
  requestCredentials: RequestCredentials,
  requestHeaders?: HeadersInit,
): RequestInit {
  return {
    cache: "no-store",
    credentials: requestCredentials,
    headers: requestHeaders,
    redirect: redirectForCredentials(requestCredentials),
  };
}

// Narrow `self` to the dedicated-worker scope so postMessage/onmessage type cleanly under the combined DOM +.
const ctx = self as unknown as DedicatedWorkerGlobalScope;
let credentials: RequestCredentials = "same-origin";
let headers: HeadersInit | undefined;

let measureSeq = 0;
let progress = progressWindow(0, REPORT_GAP_MS);

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    progress = progressWindow(performance.now(), REPORT_GAP_MS);
    credentials = msg.credentials ?? "same-origin";
    headers = msg.headers;
    measureSeq = 0;
    progress.reset();
    void run(msg.url);
  } else if (msg.type === "measure") {
    measureSeq = msg.seq;
    progress.reset();
  }
};

const post = (m: OutMsg) => ctx.postMessage(m);

function postProgress(delta: ProgressDelta | null): void {
  if (delta) post({ type: "progress", ...delta, seq: measureSeq });
}

async function run(url: string): Promise<void> {
  // Re-fetch until the measured window ends, even when one response reaches Content-Length.
  for (;;) {
    const count = (n: number): void => {
      const now = performance.now();
      postProgress(progress.add(n, now));
    };
    try {
      const res = await fetch(url, downloadFetchInit(credentials, headers));
      if (authenticationRequired(res)) {
        post({ type: "auth-required" });
        return;
      }
      if (!res.ok || !res.body) {
        post({
          type: "error",
          recoverable: recoverableDownloadStatus(res.status),
          detail: `HTTP ${res.status}`,
        });
        return;
      }
      await readBody(res.body, count);
      postProgress(progress.flush()); // the window's remainder
    } catch (err) {
      // A read that failed on an expired session is an auth failure, not a transport one, so the session is.
      if (
        credentials === "include" &&
        (await sessionAuthenticationRequired(self.location.origin))
      ) {
        post({ type: "auth-required" });
        return;
      }
      postProgress(progress.flush());
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}

/* A BYOB reader over the body, or null when the body is not a byte stream. */
function byobReader(
  body: ReadableStream<Uint8Array>,
): ReadableStreamBYOBReader | null {
  try {
    return body.getReader({ mode: "byob" });
  } catch {
    return null;
  }
}

/* Read a response body to completion, feeding each chunk's byte count to `count`. */
async function readBody(
  body: ReadableStream<Uint8Array>,
  count: (n: number) => void,
): Promise<void> {
  const byob = byobReader(body);
  if (byob) {
    let buf = new ArrayBuffer(READ_BUF_BYTES);
    for (;;) {
      const chunk = await byob.read(new Uint8Array(buf));
      if (chunk.done) break;
      if (chunk.value.byteLength) count(chunk.value.byteLength);
      // Reusing read()'s returned backing store keeps the loop allocation-free.
      buf = chunk.value.buffer as ArrayBuffer;
    }
    return;
  }
  const reader = body.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    if (value) count(value.byteLength);
  }
}
