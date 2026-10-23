// One HTTP fetch lane (api/upload.md): a counted download loop or an upload loop of sized POSTs.
import { release } from "../../protocol/documents";
import type { LaneCommand, LaneReport } from "../../protocol/messages";
import {
  ByteReports,
  countBytes,
  largestPost,
  MAX_POST_BYTES,
  partBytes,
  randomBlock,
} from "../lanes/bytes";
import { downloadRefused, lost, refused } from "../lanes/endings";

/** A POST sends for at least this long: each answer idles its lane for a turnaround the other lanes cannot fully cover. */
const TARGET_POST_MS = 2_000;
/** Round trips a POST sends for, as each answer idles its lane for about one: the lane stays 90 % busy. */
const TURNAROUNDS = 9;
/** The first and smallest POST, below which per-request overhead dominates. */
const MIN_POST_BYTES = 128 * 1024;
/** An upload's answer is a short record; a peer that sends more is not kept waiting for. */
const MAX_ANSWER_BYTES = 64 * 1024;
/** Bodies last at least this long: while the stage's end is unknown, so the last ends in its warmup, and past its end, so a stop waits for one. */
const SHORT_BODY_MS = 250;
/** A stop waits for a body ending this soon; a later one, as an early finish leaves, is cut. */
const STOP_WAIT_MS = 500;
/** A body that ends early below this came from something else, such as a proxy's page; a longer one is fetched again. */
const MIN_DOWNLOAD_BYTES = 1024 * 1024;

const post = (report: LaneReport) => postMessage(report);
const ignore = () => {};
const reports = new ByteReports((bytes, epoch) => post({ type: "bytes", bytes, epoch }));
let body = new Blob();
let partSize = 0;
/** Epoch ms by which the server ends each body, or null while the stage prepares. */
let until: number | null = null;
/** Page ms the current body ends at on the server's clock, or null while no download runs. */
let bodyEnd: number | null = null;
let stopping = false;

self.onmessage = ({ data }: MessageEvent<LaneCommand>) => {
  if (data.type === "measure") return reports.measure(data.epoch, data.at - performance.timeOrigin);
  if (data.type === "flush") return post(reports.flushed(data.at));
  if (data.type === "until") return void (until = data.until);
  if (data.type === "stop") {
    stopping = true;
    if (bodyEnd === null || bodyEnd - performance.now() > STOP_WAIT_MS) post({ type: "stopped" });
    return;
  }
  // A rejection in a worker never reaches the owner's error handler, so it ends the lane as a break.
  void (data.type === "download" ? download(data) : upload(data)).then(post, (error) =>
    post({ type: "broken", detail: String(error) }),
  );
};

async function download(command: Extract<LaneCommand, { type: "download" }>): Promise<LaneReport> {
  until = command.until;
  try {
    return await bodies(command);
  } finally {
    bodyEnd = null;
  }
}

/** Bodies fetched again until the lane stops: timed ones end on the server's clock at `until`, and a stop takes the
 *  current one's end; an untimed one is cut. */
async function bodies({
  url,
  request,
  epoch,
  streams,
  timed,
}: Extract<LaneCommand, { type: "download" }>): Promise<LaneReport> {
  reports.measure(epoch);
  reports.share(streams);
  const init: RequestInit = { ...request, cache: "no-store" };
  for (;;) {
    try {
      const sentAt = performance.now();
      const ms = !timed
        ? Infinity
        : until === null
          ? SHORT_BODY_MS
          : Math.ceil(Math.max(SHORT_BODY_MS, until - performance.timeOrigin - sentAt));
      bodyEnd = timed ? sentAt + ms : null;
      const response = await fetch(timed ? `${url}&ms=${ms}` : url, init);
      if (!response.ok || !response.body) {
        release(response);
        reports.flush();
        return { type: "ended", ending: downloadRefused(response) };
      }
      let read = 0;
      await countBytes(response.body, (bytes) => {
        read += bytes;
        reports.count(bytes);
      });
      reports.flush();
      if (stopping) return { type: "stopped" };
      // Fetching again at once would spin as fast as the short answers come; the owner paces a reopen.
      if (performance.now() - sentAt < ms && read < MIN_DOWNLOAD_BYTES)
        return { type: "ended", ending: lost("the download ended early") };
    } catch (error) {
      // Bytes counted before the break still count.
      reports.flush();
      return { type: "broken", detail: String(error) };
    }
  }
}

/** Reads an answer to its end without keeping it; false, the rest canceled, once past MAX_ANSWER_BYTES. */
async function drained(response: Response): Promise<boolean> {
  const reader = response.body?.getReader();
  if (!reader) return true;
  let seen = 0;
  try {
    for (let read = await reader.read(); !read.done; read = await reader.read())
      if ((seen += read.value.byteLength) > MAX_ANSWER_BYTES) {
        reader.cancel().catch(ignore);
        return false;
      }
  } catch {}
  return true;
}

const repeated = (part: Blob) =>
  new Blob(Array.from({ length: Math.ceil(MAX_POST_BYTES / part.size) }, () => part));

async function upload({
  url,
  request,
  streams,
  sharing,
  rttMs,
}: Extract<LaneCommand, { type: "upload" }>): Promise<LaneReport> {
  const largest = largestPost(
    streams,
    sharing,
    (navigator as { deviceMemory?: number }).deviceMemory,
  );
  const size = partBytes(largest);
  if (size !== partSize) {
    partSize = size;
    const block = new Blob([randomBlock()]);
    body = repeated(block);
    const part = new Blob(Array.from({ length: size / block.size }, () => block));
    void new Response(part)
      .blob()
      .then((file) => size === partSize && (body = repeated(file)), ignore);
  }
  let bytes = MIN_POST_BYTES;
  let rate = 0;
  // No POST returns sooner than its round trip, so the quickest one bounds it where the server's RTT is unknown.
  let turnaround = rttMs ?? Infinity;
  for (;;) {
    const startedAt = performance.now();
    try {
      const response = await fetch(url, {
        ...request,
        method: "POST",
        body: body.slice(0, bytes),
      });
      // An unread echo pins the keep-alive connection the next POST needs; it is counted, never kept.
      if (!(await drained(response)))
        return { type: "broken", detail: "the upload answer was too large" };
      if (!response.ok) return { type: "ended", ending: refused(response) };
      const elapsedMs = performance.now() - startedAt;
      post({ type: "posted", bytes, elapsedMs });
      if (!(elapsedMs > 0)) continue;
      turnaround = Math.min(turnaround, elapsedMs);
      // The rate counts each POST's turnaround as time, so a POST sized to its send time plus one turnaround sends for the send time.
      rate = rate ? 0.3 * ((bytes * 1000) / elapsedMs) + 0.7 * rate : (bytes * 1000) / elapsedMs;
      const sendMs = Math.max(TARGET_POST_MS, TURNAROUNDS * turnaround);
      const want = (rate * (sendMs + turnaround)) / 1000;
      bytes = Math.floor(
        Math.min(largest, Math.max(MIN_POST_BYTES, bytes / 2, Math.min(bytes * 2, want))),
      );
    } catch (error) {
      return { type: "broken", detail: String(error) };
    }
  }
}
