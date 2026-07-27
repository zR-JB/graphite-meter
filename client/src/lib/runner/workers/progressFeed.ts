// The server-authoritative upload feed, read the same way whichever transport
// carries it. A byte stream has no record boundaries of its own, so framing is
// this module's job; the records themselves are NDJSON.

/** What one feed reports, normalised from the wire records. */
export type ProgressEvent =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string };

/** Carried across reconnects by the caller: a replacement feed for the same
 *  upload must not report fewer bytes than its predecessor already did. */
export interface ProgressFeedState {
  lastN: number;
}

interface ProgressRecord {
  type?: string;
  bytes?: number;
  nanos?: number;
  message?: string;
}

/** Why a feed stopped. `eof` is the stream ending without a terminal record,
 *  which is a dropped feed rather than a finished upload. */
export type ProgressFeedEnd = "complete" | "fatal" | "eof";

export async function readProgressFeed(
  body: ReadableStream<Uint8Array>,
  state: ProgressFeedState,
  emit: (event: ProgressEvent) => void,
): Promise<ProgressFeedEnd> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let partial = "";
  let opened = false;
  for (;;) {
    const { value, done } = await reader.read();
    partial += decoder.decode(value, { stream: !done });
    const lines = partial.split("\n");
    partial = lines.pop() ?? "";
    for (const line of lines) {
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
    if (done) return "eof";
  }
}
