// One HTTP fetch lane (api/upload.md): a download loop counting what it reads, or an upload loop of sized POSTs that
// the server counts. Its owner reopens it after an ending and stops it by termination.
import type { LaneCommand, LaneReport } from "../../protocol/messages";
import { ByteReports, countBytes, largestPost, MAX_POST_BYTES, randomBlock } from "../lanes/bytes";
import { downloadRefused, lost, refused } from "../lanes/endings";

const MIB = 1024 * 1024;
/** A POST spans about this long: its turnaround sits in the receiver's elapsed time, so shorter ones lose accuracy. */
const TARGET_POST_MS = 500;
/** The first and smallest POST, below which per-request overhead dominates. */
const MIN_POST_BYTES = 128 * 1024;
/** An upload's answer is a short record; a peer that sends more is not kept waiting for. */
const MAX_ANSWER_BYTES = 64 * 1024;
/** A download asks for far more; a body that ends below this came from something else, such as a proxy's page. */
const MIN_DOWNLOAD_BYTES = MIB;

const post = (report: LaneReport) => postMessage(report);
const ignore = () => {};
const reports = new ByteReports((bytes, epoch) => post({ type: "bytes", bytes, epoch }));
let body: Blob | undefined;

onmessage = ({ data }: MessageEvent<LaneCommand>) => {
  if (data.type === "measure") return reports.measure(data.epoch);
  if (data.type === "flush") {
    reports.flush();
    return post({ type: "flushed" });
  }
  // A rejection in a worker never reaches the owner's error handler, so it ends the lane as a break.
  void (data.type === "download" ? download(data) : upload(data)).then(post, (error) =>
    post({ type: "broken", detail: String(error) }),
  );
};

async function download({ url, request, epoch }: Extract<LaneCommand, { type: "download" }>): Promise<LaneReport> {
  reports.measure(epoch);
  const init: RequestInit = { ...request, cache: "no-store" };
  // A response that reaches its length is fetched again until the lane ends.
  for (;;) {
    try {
      const response = await fetch(url, init);
      if (!response.ok || !response.body) {
        response.body?.cancel().catch(ignore);
        reports.flush();
        return { type: "ended", ending: downloadRefused(response) };
      }
      let read = 0;
      await countBytes(response.body, (bytes) => {
        read += bytes;
        reports.count(bytes);
      });
      // Fetching again at once would spin as fast as the short answers come; the owner's backoff paces a reopen.
      if (read < MIN_DOWNLOAD_BYTES) {
        reports.flush();
        return { type: "ended", ending: lost("the download ended early") };
      }
    } catch (error) {
      // Bytes counted before the break still count.
      reports.flush();
      return { type: "broken", detail: String(error) };
    }
  }
}

/** Reads an answer to its end without keeping it; false, and the rest cancelled, once it passes MAX_ANSWER_BYTES. A
    failed read is the next request's to find. */
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

async function upload({ url, request, streams }: Extract<LaneCommand, { type: "upload" }>): Promise<LaneReport> {
  const largest = largestPost(streams, (navigator as { deviceMemory?: number }).deviceMemory);
  if (!body) {
    const block = new Blob([randomBlock()]);
    body = new Blob(Array.from({ length: MAX_POST_BYTES / block.size }, () => block));
  }
  let bytes = MIN_POST_BYTES;
  let rate = 0;
  for (;;) {
    const startedAt = performance.now();
    try {
      const response = await fetch(url, { ...request, method: "POST", cache: "no-store", body: body.slice(0, bytes) });
      // An unread echo pins the keep-alive connection the next POST needs; it is counted, never kept.
      if (!(await drained(response))) return { type: "broken", detail: "the upload answer was too large" };
      if (!response.ok) return { type: "ended", ending: refused(response) };
      const elapsedMs = performance.now() - startedAt;
      post({ type: "posted", bytes, elapsedMs });
      if (!(elapsedMs > 0)) continue;
      rate = rate ? 0.3 * ((bytes * 1000) / elapsedMs) + 0.7 * rate : (bytes * 1000) / elapsedMs;
      const want = (rate * TARGET_POST_MS) / 1000;
      bytes = Math.floor(Math.min(largest, Math.max(MIN_POST_BYTES, bytes / 2, Math.min(bytes * 2, want))));
    } catch (error) {
      return { type: "broken", detail: String(error) };
    }
  }
}
