// Reads a receiver's feed (api/upload.md): JSON lines of at most 64 Ki UTF-16 units, blank lines as heartbeats.
import type { FeedRecord } from "../../protocol/messages";
import { progressRecord, type ProgressRecord } from "../../protocol/progress";

const MAX_RECORD = 64 * 1024;
/** A finalizing DELETE gets this long, and so does the terminal record after it. */
export const FINAL_GRACE_MS = 1_000;

type Terminal = Extract<ProgressRecord, { type: "complete" | "error" }>;

const oversized = () => new Error("upload progress record exceeds 64 Ki characters");

function parse(line: string): ProgressRecord | null {
  if (line.length > MAX_RECORD) throw oversized();
  if (!line.trim()) return null;
  try {
    return progressRecord(JSON.parse(line));
  } catch {
    return null;
  }
}

/** Emits each observation of `body` and resolves with its terminal record, or null; always releases the stream. */
export async function readRecords(
  body: ReadableStream<Uint8Array>,
  emit: (record: FeedRecord) => void,
  heard?: () => void,
): Promise<Terminal | null> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let text = "";
  try {
    for (;;) {
      const { value, done } = await reader.read();
      heard?.();
      // What is left holds no newline, so a trickled record is scanned once.
      const scanned = text.length;
      text += decoder.decode(value, { stream: !done });
      let start = 0;
      for (let end = text.indexOf("\n", scanned); end >= 0; end = text.indexOf("\n", start)) {
        const record = parse(text.slice(start, end));
        start = end + 1;
        if (!record) continue;
        if (record.type === "error") return record;
        emit(record);
        if (record.type === "complete") return record;
      }
      text = text.slice(start);
      if (text.length > MAX_RECORD) throw oversized();
      if (done) return null;
    }
  } finally {
    reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
