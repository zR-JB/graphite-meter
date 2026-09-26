// The server-authoritative upload feed, read the same way whichever transport carries it.
import type { RecoveryCause } from "../contract";

/** What one feed reports, normalised from the wire records. */
export type ProgressEvent =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string; cause: RecoveryCause };

/* Carried across reconnects by the caller: a replacement feed must not regress either receiver counter. */
export interface ProgressFeedState {
  lastN: number;
  lastT: number;
}

/** Receiver bytes and elapsed receiver nanoseconds for one upload aggregate. */
type UploadProgressRecord =
  | { type: "ready" }
  | { type: "error"; message?: string; code?: string }
  | { type: "progress" | "complete"; bytes: number; nanos: number };

// UTF-16 code units: bound retained text and JSON parsing for tiny control records.
const MAX_RECORD_LENGTH = 64 * 1024;
const REFUSALS: Record<string, RecoveryCause> = {
  invalid: "unknown-upload-id",
  ownerMismatch: "owner-mismatch",
  globalFull: "capacity-refusal",
  clientFull: "capacity-refusal",
};

/** Only explicit protocol evidence classifies a refusal, carried alike by HTTP and WebTransport. */
export function classifyUploadFailure(
  status?: number,
  code?: string | null,
): RecoveryCause {
  if (code && Object.hasOwn(REFUSALS, code)) return REFUSALS[code];
  if (status === 401) return "authentication-failure";
  if (status === 429 || status === 503) return "capacity-refusal";
  return "protocol-refusal";
}

const oversized = () =>
  new Error("upload progress record exceeds 64 Ki characters");
const counter = (value: unknown): value is number =>
  Number.isSafeInteger(value) && (value as number) >= 0;
const optionalText = (value: unknown) =>
  value === undefined || typeof value === "string";

/** Missing data is invalid; explicit zero counters remain a valid observation. */
export function decodeUploadProgress(
  value: unknown,
): UploadProgressRecord | null {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    return null;
  const raw = value as Record<string, unknown>;
  if (raw.type === "ready") return { type: "ready" };
  if (raw.type === "error")
    return optionalText(raw.message) && optionalText(raw.code)
      ? {
          type: "error",
          message: raw.message as string | undefined,
          code: raw.code as string | undefined,
        }
      : null;
  if (raw.type !== "progress" && raw.type !== "complete") return null;
  return counter(raw.bytes) && counter(raw.nanos)
    ? { type: raw.type, bytes: raw.bytes, nanos: raw.nanos }
    : null;
}

export async function readProgressFeed(
  body: ReadableStream<Uint8Array>,
  state: ProgressFeedState,
  emit: (event: ProgressEvent) => void,
): Promise<"complete" | "fatal" | "eof"> {
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
        if (line.length > MAX_RECORD_LENGTH) throw oversized();
        if (line.trim() === "") continue;
        let record: UploadProgressRecord | null = null;
        try {
          record = decodeUploadProgress(JSON.parse(line));
        } catch {
          // A truncated or non-JSON line is never a measurement.
        }
        if (record?.type === "ready" && !opened) {
          opened = true;
          emit({ type: "open" });
        } else if (record?.type === "error") {
          emit({
            type: "fatal",
            detail: record.message || "upload progress error",
            cause: classifyUploadFailure(undefined, record.code),
          });
          return "fatal";
        } else if (
          (record?.type === "progress" || record?.type === "complete") &&
          record.bytes >= state.lastN &&
          record.nanos >= state.lastT
        ) {
          const { bytes: n, nanos: t } = record;
          [state.lastN, state.lastT] = [n, t];
          emit({
            type: record.type === "progress" ? "bytes" : "complete",
            n,
            t,
          });
          if (record.type === "complete") return "complete";
        }
      }
      if (partial.length > MAX_RECORD_LENGTH) throw oversized();
      if (done) return "eof";
    }
  } finally {
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
