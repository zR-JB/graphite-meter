/* Streams server-authoritative upload snapshots from the selected throughput
 * target. Empty NDJSON lines are liveness heartbeats, never measurements. */

import { nextBackoff } from "./backoff";
import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";

type InMsg =
  | {
      type: "start";
      url: string;
      headers?: Record<string, string>;
      csrf?: Record<string, string>;
      credentials?: RequestCredentials;
    }
  | { type: "stop" };
type OutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string }
  | { type: "stall"; detail: string }
  | { type: "resume" }
  | { type: "auth-required" };
type ProgressFrame = {
  type: "ready" | "progress" | "complete" | "error";
  bytes?: number;
  nanos?: number;
  message?: string;
};

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (message: OutMsg): void => ctx.postMessage(message);
const RECONNECT_MIN_MS = 100;
const RECONNECT_MAX_MS = 2000;

let url = "";
let headers: Record<string, string> = {};
let csrf: Record<string, string> = {};
let credentials: RequestCredentials = "same-origin";
let controller: AbortController | null = null;
let wakeReconnect: (() => void) | null = null;
let stopped = false;
let finishing = false;
let stalledOut = false;
let backoff = 0;
let lastN = 0;

export function terminalProgressStatus(status: number): boolean {
  return (status >= 400 && status < 500) || status === 503;
}

ctx.onmessage = (event: MessageEvent<InMsg>): void => {
  if (event.data.type === "start") {
    url = event.data.url;
    headers = event.data.headers ?? {};
    csrf = event.data.csrf ?? {};
    credentials = event.data.credentials ?? "same-origin";
    stopped = false;
    finishing = false;
    lastN = 0;
    void run();
  } else {
    void finish();
  }
};

async function run(): Promise<void> {
  while (!stopped) {
    controller = new AbortController();
    let detail = "progress stream closed";
    try {
      const response = await fetch(url, {
        cache: "no-store",
        headers: { ...headers, accept: "application/x-ndjson" },
        signal: controller.signal,
        credentials,
        redirect: redirectForCredentials(credentials),
      });
      if (authenticationRequired(response)) {
        post({ type: "auth-required" });
        stopped = true;
        return;
      }
      if (!response.ok) {
        if (terminalProgressStatus(response.status)) {
          post({
            type: "fatal",
            detail: `progress returned HTTP ${response.status}`,
          });
          stopped = true;
          return;
        }
        throw new Error(`progress returned HTTP ${response.status}`);
      }
      if (!response.body)
        throw new Error(`progress returned HTTP ${response.status}`);
      await readEvents(response.body);
    } catch (error) {
      if (
        !stopped &&
        credentials === "include" &&
        (await sessionAuthenticationRequired(self.location.origin))
      ) {
        stopped = true;
        post({ type: "auth-required" });
        return;
      }
      detail = String(error);
    } finally {
      controller = null;
    }
    if (stopped) return;
    if (!stalledOut) {
      post({ type: "stall", detail });
      stalledOut = true;
    }
    backoff = nextBackoff(backoff, RECONNECT_MIN_MS, RECONNECT_MAX_MS);
    await reconnectDelay(backoff);
  }
}

/** Sleep before the next reconnect attempt, cut short if `wakeReconnect` fires. */
function reconnectDelay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(wake, ms);
    function wake(): void {
      clearTimeout(timer);
      if (wakeReconnect === wake) wakeReconnect = null;
      resolve();
    }
    wakeReconnect = wake;
  });
}

async function readEvents(body: ReadableStream<Uint8Array>): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let partialLine = "";
  for (;;) {
    const { value, done } = await reader.read();
    partialLine += decoder.decode(value, { stream: !done });
    const lines = partialLine.split("\n");
    partialLine = lines.pop() ?? "";
    for (const line of lines) {
      if (line.trim() === "") continue;
      let event: ProgressFrame;
      try {
        event = JSON.parse(line) as ProgressFrame;
      } catch {
        continue; // a truncated or non-JSON line is never a measurement
      }
      if (event.type === "ready") {
        backoff = 0;
        if (stalledOut) {
          post({ type: "resume" });
          stalledOut = false;
        }
        post({ type: "open" });
      } else if (event.type === "progress" || event.type === "complete") {
        const n = Number(event.bytes ?? 0);
        const t = Number(event.nanos ?? 0);
        if (n >= lastN) lastN = n;
        post({
          type: event.type === "progress" ? "bytes" : "complete",
          n: lastN,
          t,
        });
        if (event.type === "complete") {
          teardown();
          return;
        }
      } else if (event.type === "error") {
        post({
          type: "fatal",
          detail: event.message || "upload progress error",
        });
        stopped = true;
        return;
      }
    }
    if (done) return;
  }
}

/** Handle `stop`: ask the server to close the upload session. When the DELETE
 *  lands, the stream itself delivers the final `complete` frame and tears down
 *  there; when it does not, tear down locally so nothing keeps reconnecting. */
async function finish(): Promise<void> {
  if (stopped || finishing) return;
  finishing = true;
  wakeReconnect?.();
  try {
    const response = await fetch(url, {
      method: "DELETE",
      cache: "no-store",
      headers: { ...headers, ...csrf },
      credentials,
      redirect: redirectForCredentials(credentials),
    });
    if (authenticationRequired(response)) {
      post({ type: "auth-required" });
      teardown();
      return;
    }
    if (!response.ok) teardown();
  } catch {
    teardown();
  }
}

function teardown(): void {
  stopped = true;
  wakeReconnect?.();
  wakeReconnect = null;
  controller?.abort();
  controller = null;
}
