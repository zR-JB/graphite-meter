/* ============================================================
 * The Graphite Meter: WebTransport transfer worker
 * ============================================================
 * One worker per direction, owning the session and every lane stream on it.
 * A WebTransport object cannot be transferred and a transferred stream still
 * pumps its chunks through the owning realm, so lanes cannot be split across
 * workers the way fetch lanes are.
 *
 * Streams carry raw bytes end to end; the session URL carries every parameter.
 * The server opens the download lanes and the upload progress feed, so this
 * worker reads incoming streams for both and only opens the upload lanes.
 * ============================================================ */

import { mintWtToken, withWtToken, type WtMint } from "./wtToken";

/** Records of the server's upload feed, relayed verbatim to the main thread. */
type ProgressMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string };

type InMsg =
  | {
      type: "start";
      url: string;
      dir: "down" | "up";
      lanes: number;
      datagrams: boolean;
      mint?: WtMint;
      progressUrl?: string;
      headers?: Record<string, string>;
      credentials?: RequestCredentials;
    }
  | { type: "measure"; seq: number }
  | { type: "stop" };

type OutMsg =
  | { type: "established" }
  | { type: "progress"; bytes: number; elapsedMs: number; seq: number }
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "upload-progress"; msg: ProgressMsg }
  | { type: "stopped" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg): void => ctx.postMessage(m);

/** Download report cadence, matching the fetch lanes. */
const REPORT_GAP_MS = 50;
/** Upload write size; the stream's own flow control paces the lane. */
const UPLOAD_CHUNK_BYTES = 1024 * 1024;
/** crypto.getRandomValues' hard per-call byte quota. */
const RNG_CHUNK_BYTES = 65536;
/** A session that has not become ready by now never will on this network. */
const ESTABLISH_TIMEOUT_MS = 3000;
/** Grace for the terminal progress record after the finalizing DELETE. */
const COMPLETE_GRACE_MS = 1000;
/** Upload alive cadence toward the main thread. A datagram loop iterates per
 *  packet, so an unthrottled alive would jank the thread latency is measured on. */
const ALIVE_GAP_MS = 250;

let lastAlive = 0;

function postAlive(): void {
  const now = performance.now();
  if (now - lastAlive < ALIVE_GAP_MS) return;
  lastAlive = now;
  post({ type: "alive" });
}

let session: WebTransport | null = null;
let stopped = false;
let measureSeq = 0;
/** Bytes read since the last report, and when that window opened. */
let windowBytes = 0;
let windowStart = 0;

ctx.onmessage = (e: MessageEvent<InMsg>): void => {
  const msg = e.data;
  switch (msg.type) {
    case "start":
      stopped = false;
      windowBytes = 0;
      windowStart = performance.now();
      void run(msg);
      break;
    case "measure":
      measureSeq = msg.seq;
      windowBytes = 0;
      windowStart = performance.now();
      break;
    case "stop":
      void shutdown();
      break;
  }
};

async function run(msg: Extract<InMsg, { type: "start" }>): Promise<void> {
  const token = await mintWtToken(msg.mint);
  if (stopped) return;
  try {
    session = new WebTransport(withWtToken(msg.url, token), {
      congestionControl: "throughput",
    });
    await Promise.race([
      session.ready,
      new Promise((_, reject) =>
        setTimeout(
          () => reject(new Error("webtransport session did not establish")),
          ESTABLISH_TIMEOUT_MS,
        ),
      ),
    ]);
  } catch (err) {
    post({ type: "error", recoverable: true, detail: String(err) });
    return;
  }
  post({ type: "established" });
  void session.closed.catch(() => fail(true, "webtransport session closed"));
  if (msg.dir === "down") {
    if (msg.datagrams) {
      void readDatagrams();
      return;
    }
    void acceptDownloadStreams();
    return;
  }
  // The feed is read first, so the server is already reporting when bytes start.
  if (!openProgress(msg.progressUrl, msg.headers, msg.credentials)) return;
  if (msg.datagrams) {
    void uploadDatagrams();
    return;
  }
  const block = incompressibleBlock();
  for (let i = 0; i < msg.lanes; i++) void uploadLane(block);
}

/** Drain every server-opened stream: each is one sized lane request, replaced
 *  by the server when exhausted, so the accept loop runs for the whole stage. */
async function acceptDownloadStreams(): Promise<void> {
  if (!session) return;
  const incoming = session.incomingUnidirectionalStreams.getReader();
  try {
    for (;;) {
      const { value, done } = await incoming.read();
      if (done || stopped) return;
      void drainLane(value as ReadableStream);
    }
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

async function drainLane(lane: ReadableStream): Promise<void> {
  const reader = lane.getReader();
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done || stopped) return;
      countDownload((value as Uint8Array).byteLength);
    }
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** Experimental: the query asked for the request as datagrams; count what
 *  lands. Loss shows up as missing goodput, since nothing is retransmitted. */
async function readDatagrams(): Promise<void> {
  if (!session) return;
  try {
    const reader = session.datagrams.readable.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (done || stopped) return;
      countDownload((value as Uint8Array).byteLength);
    }
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** Experimental: flood path-MTU-sized datagrams. The server counts what arrives
 *  and the progress feed reports it, as with the stream lanes. */
async function uploadDatagrams(): Promise<void> {
  if (!session) return;
  try {
    const writer = session.datagrams.writable.getWriter();
    const payload = new Uint8Array(session.datagrams.maxDatagramSize);
    crypto.getRandomValues(payload);
    while (!stopped) {
      await writer.ready;
      await writer.write(payload);
      postAlive();
    }
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** Report at the fetch lanes' cadence: one aggregate for the whole session,
 *  since the main thread treats this worker as a single lane. */
function countDownload(n: number): void {
  windowBytes += n;
  const now = performance.now();
  const elapsedMs = now - windowStart;
  if (elapsedMs < REPORT_GAP_MS) return;
  post({ type: "progress", bytes: windowBytes, elapsedMs, seq: measureSeq });
  windowBytes = 0;
  windowStart = now;
}

/** One upload lane: a unidirectional stream written for the whole stage. The
 *  writer's backpressure is the pacing loop, so no sizing is needed. */
async function uploadLane(block: Uint8Array<ArrayBuffer>): Promise<void> {
  if (!session) return;
  try {
    const writer = (await session.createUnidirectionalStream()).getWriter();
    while (!stopped) {
      await writer.ready;
      await writer.write(block);
      postAlive();
    }
    await writer.close();
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** Read the one stream the server opens on an upload session as the progress
 *  feed and relay its records. */
function openProgress(
  progressUrl?: string,
  headers?: Record<string, string>,
  credentials?: RequestCredentials,
): boolean {
  if (!session || !progressUrl) {
    fail(false, "upload progress route missing");
    return false;
  }
  const incoming = session.incomingUnidirectionalStreams.getReader();
  void incoming
    .read()
    .then(({ value, done }) => {
      if (done || !value) throw new Error("no progress stream");
      return readProgress(value as ReadableStream);
    })
    .catch((err: unknown) => {
      if (!stopped)
        post({
          type: "upload-progress",
          msg: { type: "fatal", detail: String(err) },
        });
    });
  finalize = async (): Promise<void> => {
    await fetch(progressUrl, {
      method: "DELETE",
      cache: "no-store",
      headers,
      credentials,
    }).catch(() => {});
  };
  return true;
}

async function readProgress(readable: ReadableStream): Promise<void> {
  const reader = readable.getReader();
  const decoder = new TextDecoder();
  let buffered = "";
  let opened = false;
  for (;;) {
    const chunk = await reader.read();
    if (chunk.done) return;
    buffered += decoder.decode(chunk.value as AllowSharedBufferSource, {
      stream: true,
    });
    let cut = buffered.indexOf("\n");
    for (; cut >= 0; cut = buffered.indexOf("\n")) {
      const line = buffered.slice(0, cut).trim();
      buffered = buffered.slice(cut + 1);
      if (line === "") continue; // heartbeat
      const record = parseRecord(line);
      if (!record) continue;
      if (record.type === "ready") {
        if (!opened) {
          opened = true;
          post({ type: "upload-progress", msg: { type: "open" } });
        }
        continue;
      }
      if (record.type === "error") {
        post({
          type: "upload-progress",
          msg: { type: "fatal", detail: record.message ?? "upload refused" },
        });
        return;
      }
      post({
        type: "upload-progress",
        msg: {
          type: record.type === "complete" ? "complete" : "bytes",
          n: record.bytes ?? 0,
          t: record.nanos ?? 0,
        },
      });
      if (record.type === "complete") return;
    }
  }
}

interface ProgressRecord {
  type: string;
  bytes?: number;
  nanos?: number;
  message?: string;
}

function parseRecord(line: string): ProgressRecord | null {
  try {
    return JSON.parse(line) as ProgressRecord;
  } catch {
    return null;
  }
}

/** Sends the finalizing DELETE once the lanes stop; set when upload starts. */
let finalize: (() => Promise<void>) | null = null;

/** Stop the lanes, finalize the upload, let the terminal progress record land,
 *  and ack, so the main thread can terminate this worker deterministically. */
async function shutdown(): Promise<void> {
  if (stopped) return;
  stopped = true;
  if (finalize) {
    await finalize();
    await new Promise((resolve) => setTimeout(resolve, COMPLETE_GRACE_MS));
  }
  session?.close();
  session = null;
  post({ type: "stopped" });
}

function fail(recoverable: boolean, detail: string): void {
  if (stopped) return;
  post({ type: "error", recoverable, detail });
}

/** One incompressible block, filled in 64 KiB chunks (the getRandomValues
 *  per-call quota) and rewritten for the whole stage. */
function incompressibleBlock(): Uint8Array<ArrayBuffer> {
  const block = new Uint8Array(new ArrayBuffer(UPLOAD_CHUNK_BYTES));
  for (let off = 0; off < block.length; off += RNG_CHUNK_BYTES) {
    crypto.getRandomValues(
      block.subarray(off, Math.min(off + RNG_CHUNK_BYTES, block.length)),
    );
  }
  return block;
}
