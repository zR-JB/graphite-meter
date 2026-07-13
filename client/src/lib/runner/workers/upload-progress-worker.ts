/* Streams server-authoritative upload snapshots from the selected throughput
 * target. Empty NDJSON lines are liveness heartbeats, never measurements. */

import { nextBackoff } from "./backoff";

type InMsg =
  | { type: "start"; url: string; headers?: Record<string, string> }
  | { type: "stop" };
type OutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "stall"; detail: string }
  | { type: "resume" };
type ProgressEvent = {
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
let controller: AbortController | null = null;
let wakeReconnect: (() => void) | null = null;
let stopped = false;
let finishing = false;
let stalledOut = false;
let backoff = 0;
let lastN = 0;

ctx.onmessage = (event: MessageEvent<InMsg>): void => {
  if (event.data.type === "start") {
    url = event.data.url;
    headers = event.data.headers ?? {};
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
      });
      if (!response.ok || !response.body)
        throw new Error(`progress returned HTTP ${response.status}`);
      await readEvents(response.body);
    } catch (error) {
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

function reconnectDelay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(finish, ms);
    function finish(): void {
      clearTimeout(timer);
      if (wakeReconnect === finish) wakeReconnect = null;
      resolve();
    }
    wakeReconnect = finish;
  });
}

async function readEvents(body: ReadableStream<Uint8Array>): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let pending = "";
  for (;;) {
    const { value, done } = await reader.read();
    pending += decoder.decode(value, { stream: !done });
    const lines = pending.split("\n");
    pending = lines.pop() ?? "";
    for (const line of lines) {
      if (line.trim() === "") continue;
      let event: ProgressEvent;
      try {
        event = JSON.parse(line) as ProgressEvent;
      } catch {
        continue;
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
        throw new Error(event.message || "upload progress error");
      }
    }
    if (done) return;
  }
}

async function finish(): Promise<void> {
  if (stopped || finishing) return;
  finishing = true;
  wakeReconnect?.();
  try {
    const response = await fetch(url, {
      method: "DELETE",
      cache: "no-store",
      headers,
    });
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
