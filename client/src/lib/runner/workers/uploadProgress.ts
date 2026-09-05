/** Receiver bytes and elapsed receiver nanoseconds for one upload aggregate. */
export type UploadProgressRecord =
  | { type: "ready" }
  | { type: "error"; message?: string; code?: string }
  | { type: "progress" | "complete"; bytes: number; nanos: number };

/** Missing data is invalid; explicit zero counters remain a valid observation. */
export function decodeUploadProgress(
  value: unknown,
): UploadProgressRecord | null {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    return null;
  const raw = value as Record<string, unknown>;
  switch (raw.type) {
    case "ready":
      return { type: "ready" };
    case "error":
      if (
        (raw.message !== undefined && typeof raw.message !== "string") ||
        (raw.code !== undefined && typeof raw.code !== "string")
      )
        return null;
      return {
        type: "error",
        message: raw.message as string | undefined,
        code: raw.code as string | undefined,
      };
    case "progress":
    case "complete":
      if (
        typeof raw.bytes !== "number" ||
        !Number.isSafeInteger(raw.bytes) ||
        raw.bytes < 0 ||
        typeof raw.nanos !== "number" ||
        !Number.isSafeInteger(raw.nanos) ||
        raw.nanos < 0
      )
        return null;
      return { type: raw.type, bytes: raw.bytes, nanos: raw.nanos };
    default:
      return null;
  }
}
