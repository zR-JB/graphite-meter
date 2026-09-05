// The server-authoritative upload feed, read the same way whichever transport carries it.
import {
  classifyUploadFailure,
  type UploadRefusalCode,
} from "../uploadFailure";
import type { RecoveryCause } from "../contract";

/** What one feed reports, normalised from the wire records. */
export type ProgressEvent =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string; cause: RecoveryCause };

/* Carried across reconnects by the caller: a replacement feed for the same upload must not report fewer bytes. */
export interface ProgressFeedState {
  lastN: number;
}

interface ProgressRecord {
  type?: string;
  bytes?: number;
  nanos?: number;
  message?: string;
  code?: UploadRefusalCode;
}

/* Why a feed stopped. */
type ProgressFeedEnd = "complete" | "fatal" | "eof";
// UTF-16 code units: bound retained text and JSON parsing for tiny control records.
const MAX_RECORD_LENGTH = 64 * 1024;

export async function readProgressFeed(
  body: ReadableStream<Uint8Array>,
  state: ProgressFeedState,
  emit: (event: ProgressEvent) => void,
): Promise<ProgressFeedEnd> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let partial = "";
  let opened = false;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      partial += decoder.decode(value, { stream: !done });
      const lines = partial.split("\n");
      partial = lines.pop() ?? "";
      for (const line of lines) {
        if (line.length > MAX_RECORD_LENGTH)
          throw new Error("upload progress record exceeds 64 Ki characters");
        if (line.trim() === "") continue; // heartbeat
        let record: ProgressRecord;
        try {
          record = JSON.parse(line) as ProgressRecord;
        } catch {
          continue; // a truncated or non-JSON line is never a measurement
        }
        if (record.type === "ready") {
          if (opened) continue;
          opened = true;
          emit({ type: "open" });
          continue;
        }
        if (record.type === "error") {
          emit({
            type: "fatal",
            detail: record.message || "upload progress error",
            cause: classifyUploadFailure(undefined, record.code),
          });
          return "fatal";
        }
        if (record.type !== "progress" && record.type !== "complete") continue;
        const n = Number(record.bytes ?? 0);
        if (n >= state.lastN) state.lastN = n;
        const event = record.type === "progress" ? "bytes" : "complete";
        emit({ type: event, n: state.lastN, t: Number(record.nanos ?? 0) });
        if (record.type === "complete") return "complete";
      }
      if (partial.length > MAX_RECORD_LENGTH)
        throw new Error("upload progress record exceeds 64 Ki characters");
      if (done) return "eof";
    }
  } finally {
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
