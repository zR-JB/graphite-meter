/* One worker per direction, owning the session and every lane stream on it: a
 * WebTransport object cannot be transferred, and a transferred stream still
 * pumps through the owning realm, so lanes cannot be split across workers the
 * way fetch lanes are.
 *
 * Streams carry raw bytes; the session URL carries every parameter. The server
 * opens the download lanes and the upload progress feed, so this worker reads
 * incoming streams for both and opens only the upload lanes. */

import { mintWtToken, spendWtToken, withWtToken, type WtMint } from "./wtToken";
import { ESTABLISH_BUDGET_MS, PROGRESS_FINAL_GRACE_MS } from "../real/budgets";
import { incompressibleBlock } from "./payload";
import { readProgressFeed, type ProgressEvent } from "./progressFeed";
import { ProgressWindow, type ProgressDelta } from "./progressWindow";
import { READ_BUF_BYTES, REPORT_GAP_MS } from "./tuning";

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
  | { type: "upload-progress"; msg: ProgressEvent }
  | { type: "auth-required" }
  | { type: "stopped" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const post = (m: OutMsg): void => ctx.postMessage(m);

/** Upload alive cadence toward the main thread. A datagram loop iterates per
 *  packet, so an unthrottled alive would jank the thread latency is measured on. */
const ALIVE_GAP_MS = 250;

/** Longest a datagram loop may run without a task turn. A worker's message
 *  queue is dispatched only across a task, and both datagram loops can have
 *  their sole suspension settled by the transport within one microtask
 *  checkpoint, which would leave the loop outrunning the queue carrying its own
 *  `stop`. Yielding per packet would price a turn into every packet of a
 *  measurement path; taking one on an interval bounds the starvation window to
 *  this gap at one turn per gap, whatever the packet rate. */
const YIELD_GAP_MS = 4;

/** Bytes per WebTransport stream write. */
const WRITE_CHUNK_BYTES = 4 * 1024 * 1024;

/** Session congestion control hint. */
const CONGESTION_CONTROL: WebTransportCongestionControl = "throughput";

/** One task turn, so the queue carrying `stop` is dispatched. The port hop is
 *  what keeps the timer off the HTML nesting clamp, which otherwise floors a
 *  timer re-armed from a timer's own task at 4ms. Measured as a wash on
 *  throughput — the transport buffers absorb the park — so this buys
 *  responsiveness, not rate. */
const taskTurn = (): Promise<void> =>
  new Promise((resolve) => {
    const { port1, port2 } = new MessageChannel();
    port1.onmessage = (): void => {
      port1.close();
      port2.close();
      setTimeout(resolve);
    };
    port2.postMessage(0);
  });

let lastAlive = 0;

/** `now` is passed in from a loop that has already read the clock: a datagram
 *  loop reads it per packet and must not read it twice. */
function postAlive(now = performance.now()): void {
  if (now - lastAlive < ALIVE_GAP_MS) return;
  lastAlive = now;
  post({ type: "alive" });
}

let session: WebTransport | null = null;
let stopped = false;
/** Separate from `stopped`, which a terminal refusal latches too: the owner
 *  waits on the ack whatever ended the session, so a stop must still run. */
let stopping = false;
/** Latches the first failure of this session, so its echoes stay silent. */
let failed = false;
let measureSeq = 0;
let progress = new ProgressWindow(0, REPORT_GAP_MS);
/** Sends the finalizing DELETE once the lanes stop; set when upload starts. */
let finalize: (() => Promise<void>) | null = null;
/** Resolves the shutdown grace as soon as the terminal record lands, so the
 *  stage does not sit its full length with the lanes already silent. */
let completed: (() => void) | null = null;

ctx.onmessage = (e: MessageEvent<InMsg>): void => {
  const msg = e.data;
  switch (msg.type) {
    case "start":
      stopped = false;
      stopping = false;
      failed = false;
      progress = new ProgressWindow(performance.now(), REPORT_GAP_MS);
      void run(msg);
      break;
    case "measure":
      measureSeq = msg.seq;
      progress.reset();
      break;
    case "stop":
      void shutdown();
      break;
  }
};

async function run(msg: Extract<InMsg, { type: "start" }>): Promise<void> {
  const minted = await mintWtToken(msg.mint);
  if (stopped) return;
  // An authenticated dial cannot proceed without a token. The refusal already
  // says whether the login session is gone, so a retry needs no second request.
  if (msg.mint && minted.token === "") {
    if (minted.authRequired) {
      post({ type: "auth-required" });
      stopped = true;
      return;
    }
    fail(true, "webtransport token mint failed");
    return;
  }
  const token = minted.token;
  let dialed: WebTransport;
  try {
    dialed = new WebTransport(withWtToken(msg.url, token), {
      congestionControl: CONGESTION_CONTROL,
    });
  } catch (err) {
    fail(true, String(err));
    return;
  }
  session = dialed;
  // `closed` resolves on a graceful close and rejects on an abrupt one, and the
  // server always closes gracefully, so both arms end this session's work. A
  // stop has already latched `stopped`, which keeps the report silent.
  const closed = (): void => fail(true, "webtransport session closed");
  void dialed.closed.then(closed, closed);
  try {
    await Promise.race([
      dialed.ready,
      new Promise((_, reject) =>
        setTimeout(
          () => reject(new Error("webtransport session did not establish")),
          ESTABLISH_BUDGET_MS,
        ),
      ),
    ]);
  } catch (err) {
    // A dial still in flight would otherwise outlive this worker's report.
    try {
      dialed.close();
    } catch {
      /* already closing */
    }
    fail(true, String(err));
    return;
  }
  // The race resolved on `ready`, so the server accepted the CONNECT and
  // deleted the token it carried. Reporting the spend is what keeps a later
  // dial from replaying it; a dial that failed above never reaches here, and
  // its token stays reusable.
  spendWtToken(token);
  post({ type: "established" });
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
  const block = incompressibleBlock(WRITE_CHUNK_BYTES);
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
      void drainLane(value as ReadableStream<Uint8Array>);
    }
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** The reused BYOB buffer is the read-side ceiling at multi-Gbit/s: a default
 *  reader allocates per chunk, and one worker drains every lane of the session. */
async function drainLane(lane: ReadableStream<Uint8Array>): Promise<void> {
  try {
    let byob: ReadableStreamBYOBReader | null = null;
    try {
      byob = lane.getReader({ mode: "byob" });
    } catch {
      byob = null;
    }
    if (byob) {
      let buf = new ArrayBuffer(READ_BUF_BYTES);
      for (;;) {
        const chunk = await byob.read(new Uint8Array(buf));
        if (chunk.done || stopped) return;
        if (chunk.value.byteLength) countDownload(chunk.value.byteLength);
        // read() detaches the buffer and hands back the same backing store.
        buf = chunk.value.buffer as ArrayBuffer;
      }
    }
    const reader = lane.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (done || stopped) return;
      countDownload(value.byteLength);
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
    let lastYield = performance.now();
    for (;;) {
      const { value, done } = await reader.read();
      if (done || stopped) return;
      // One clock read serves both the report window and the yield gap: a
      // datagram is ~1200 bytes, so a second read here is another ~100k calls
      // a second on the path being measured.
      const now = performance.now();
      countDownload((value as Uint8Array).byteLength, now);
      if (now - lastYield < YIELD_GAP_MS) continue;
      await taskTurn();
      lastYield = performance.now();
    }
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** Experimental: flood path-MTU-sized datagrams. The server counts what arrives
 *  and the progress feed reports it, as with the stream lanes. `ready` is the
 *  backpressure gate; the write itself is not awaited, so the queue stays at
 *  the transport's own high-water mark without a promise round trip per packet. */
async function uploadDatagrams(): Promise<void> {
  if (!session) return;
  const datagrams = session.datagrams;
  try {
    const writer = datagrams.writable.getWriter();
    const payload = incompressibleBlock(datagrams.maxDatagramSize);
    let lastYield = performance.now();
    while (!stopped) {
      await writer.ready;
      // The path MTU estimate can shrink mid-session and an oversized datagram
      // is dropped with a resolved promise, so nothing but this clamp keeps the
      // write on the path.
      const size = Math.min(payload.length, datagrams.maxDatagramSize);
      // Nothing fits any more. Returning silently would leave the stage running
      // to its full timer with zero bytes, no restart and no diagnostic.
      if (size === 0) {
        fail(true, "webtransport datagram size collapsed");
        return;
      }
      void writer.write(payload.subarray(0, size)).catch(() => {});
      const now = performance.now();
      postAlive(now);
      if (now - lastYield < YIELD_GAP_MS) continue;
      await taskTurn();
      lastYield = performance.now();
    }
  } catch (err) {
    if (!stopped) fail(true, String(err));
  }
}

/** Report at the fetch lanes' cadence: one aggregate for the whole session,
 *  since the main thread treats this worker as a single lane. `now` is passed
 *  in by a loop that has already read the clock. */
function countDownload(n: number, now = performance.now()): void {
  postProgress(progress.add(n, now));
}

function postProgress(delta: ProgressDelta | null): void {
  if (delta) post({ type: "progress", ...delta, seq: measureSeq });
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

/** Read the server's progress feed and any later refusal control stream. */
function openProgress(
  progressUrl?: string,
  headers?: Record<string, string>,
  credentials?: RequestCredentials,
): boolean {
  if (!session || !progressUrl) {
    fail(false, "upload progress route missing");
    return false;
  }
  void readProgressStreams(session.incomingUnidirectionalStreams.getReader());
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

async function readProgressStreams(
  incoming: ReadableStreamDefaultReader<unknown>,
): Promise<void> {
  try {
    let opened = false;
    for (;;) {
      const { value, done } = await incoming.read();
      if (done || !value) {
        if (!opened && !stopped) throw new Error("no progress stream");
        return;
      }
      opened = true;
      // The primary feed remains open through the stage. Keep accepting later
      // server-opened control streams concurrently so an explicit lane refusal
      // reaches the authoritative recovery classifier immediately.
      void readProgress(value as ReadableStream);
    }
  } catch (err) {
    // A transport-level break is the session dying: recoverable, the owner
    // restarts the session and the server re-opens the feed. An error record
    // inside a stream is relayed by readProgress as structural evidence.
    if (!stopped) fail(true, `upload progress stream: ${String(err)}`);
  }
}

async function readProgress(
  readable: ReadableStream<Uint8Array>,
): Promise<void> {
  // The monotonic aggregate is per feed: a replacement feed rides a replacement
  // session, and a session is a worker realm.
  const end = await readProgressFeed(readable, { lastN: 0 }, (event) =>
    post({ type: "upload-progress", msg: event }),
  );
  if (end === "complete") {
    completed?.();
    return;
  }
  // A feed ending without a terminal record is a dropped feed, not a finished
  // upload, and it resolves rather than throwing, so the transport-break path
  // never sees it. Recoverable for the same reason that one is: the owner
  // restarts the session and the server re-opens the feed.
  if (end === "eof") fail(true, "webtransport progress feed ended early");
}

/** Stop the lanes, finalize the upload, let the terminal progress record land,
 *  and ack, so the main thread can terminate this worker deterministically. */
async function shutdown(): Promise<void> {
  if (stopping) return;
  stopping = true;
  postProgress(progress.flush());
  stopped = true;
  if (finalize) {
    await finalize();
    await new Promise<void>((resolve) => {
      completed = resolve;
      setTimeout(resolve, PROGRESS_FINAL_GRACE_MS);
    });
    completed = null;
  }
  session?.close();
  session = null;
  post({ type: "stopped" });
}

/** One session death reaches every lane reader, the accept loop and the
 *  session's close promise. They describe one incident, so only the first is
 *  reported; the owner restarts the session either way. */
function fail(recoverable: boolean, detail: string): void {
  if (stopped || failed) return;
  failed = true;
  post({ type: "error", recoverable, detail });
}
