/* ============================================================
 * The Graphite Meter: WebTransport transfer worker
 * ============================================================
 * One worker per direction, owning the session and every lane stream on it.
 * A WebTransport object cannot be transferred and a transferred stream still
 * pumps its chunks through the owning realm, so lanes cannot be split across
 * workers the way fetch lanes are.
 *
 * Download opens a bidirectional stream per lane, sized by its SIZE preamble.
 * Upload writes one unidirectional stream per lane and carries the server's
 * progress feed on a bidirectional stream of the same session, so the counter
 * rides the connection under test.
 * ============================================================ */

import { encodePreamble } from "../real/wire";

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
      bytesPerLane: number;
      progressUrl?: string;
      headers?: Record<string, string>;
      credentials?: RequestCredentials;
    }
  | { type: "measure"; seq: number }
  | { type: "stop" };

type OutMsg =
  | { type: "progress"; bytes: number; elapsedMs: number; seq: number }
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "upload-progress"; msg: ProgressMsg };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg): void => ctx.postMessage(m);

/** Download report cadence, matching the fetch lanes. */
const REPORT_GAP_MS = 50;
/** Upload write size; the stream's own flow control paces the lane. */
const UPLOAD_CHUNK_BYTES = 1024 * 1024;
/** crypto.getRandomValues' hard per-call byte quota. */
const RNG_CHUNK_BYTES = 65536;
/** Grace for the terminal progress record after the finalizing DELETE. */
const COMPLETE_GRACE_MS = 1000;

let session: WebTransport | null = null;
let stopped = false;
let measureSeq = 0;
/** Bytes read since the last report, and when that window opened. */
let windowBytes = 0;
let windowStart = 0;
let progressWriter: WritableStreamDefaultWriter<Uint8Array> | null = null;

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
  try {
    session = new WebTransport(msg.url, { congestionControl: "throughput" });
    await session.ready;
  } catch (err) {
    post({ type: "error", recoverable: false, detail: String(err) });
    return;
  }
  void session.closed.catch(() => fail(true, "webtransport session closed"));
  if (msg.dir === "down") {
    for (let i = 0; i < msg.lanes; i++) void downloadLane(msg.bytesPerLane);
    return;
  }
  // The feed opens first, so the server is already reporting when bytes start.
  if (!(await openProgress(msg.progressUrl, msg.headers, msg.credentials)))
    return;
  const block = incompressibleBlock();
  for (let i = 0; i < msg.lanes; i++) void uploadLane(block);
}

/** One download lane: request bytes with a SIZE preamble, count what comes back
 *  on that same stream, and reopen when the request is exhausted. */
async function downloadLane(bytes: number): Promise<void> {
  const preamble = new TextEncoder().encode(
    encodePreamble({ op: "SIZE", bytes: BigInt(bytes) }),
  );
  while (!stopped && session) {
    try {
      const stream = await session.createBidirectionalStream();
      const writer = stream.writable.getWriter();
      await writer.write(preamble);
      await writer.close();
      const reader = stream.readable.getReader();
      for (;;) {
        const { value, done } = await reader.read();
        if (done || stopped) break;
        countDownload((value as Uint8Array).byteLength);
      }
    } catch (err) {
      if (!stopped) fail(true, String(err));
      return;
    }
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
      post({ type: "alive" });
    }
    await writer.close();
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** Open the progress feed on a bidirectional stream of this session and relay
 *  its records. The HI preamble is what announces the stream: a QUIC stream
 *  reaches the peer on its first write. */
async function openProgress(
  progressUrl?: string,
  headers?: Record<string, string>,
  credentials?: RequestCredentials,
): Promise<boolean> {
  if (!session || !progressUrl) {
    fail(false, "upload progress route missing");
    return false;
  }
  try {
    const stream = await session.createBidirectionalStream();
    progressWriter = stream.writable.getWriter();
    await progressWriter.write(
      new TextEncoder().encode(encodePreamble({ op: "HI", proto: "wt" })),
    );
    void readProgress(stream.readable);
  } catch (err) {
    fail(false, String(err));
    return false;
  }
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
    let chunk;
    try {
      chunk = await reader.read();
    } catch (err) {
      if (!stopped)
        post({
          type: "upload-progress",
          msg: { type: "fatal", detail: String(err) },
        });
      return;
    }
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

/** Stop the lanes, finalize the upload, and let the terminal progress record
 *  land before the main thread terminates this worker. */
async function shutdown(): Promise<void> {
  if (stopped) return;
  stopped = true;
  if (finalize) {
    await finalize();
    await new Promise((resolve) => setTimeout(resolve, COMPLETE_GRACE_MS));
  }
  try {
    await progressWriter?.close();
  } catch {
    /* the session is closing anyway */
  }
  session?.close();
  session = null;
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
