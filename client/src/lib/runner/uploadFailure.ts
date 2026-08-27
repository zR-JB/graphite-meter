import type { RecoveryCause } from "./contract";

/** Stable server refusal codes, carried in both HTTP and WebTransport paths. */
export type UploadRefusalCode =
  "invalid" | "globalFull" | "clientFull" | "ownerMismatch";

/* Classify only explicit protocol evidence. */
export function classifyUploadFailure(
  status?: number,
  code?: string | null,
): RecoveryCause {
  switch (code as UploadRefusalCode | undefined) {
    case "invalid":
      return "unknown-upload-id";
    case "ownerMismatch":
      return "owner-mismatch";
    case "globalFull":
    case "clientFull":
      return "capacity-refusal";
  }
  if (status === 401) return "authentication-failure";
  if (status === 429 || status === 503) return "capacity-refusal";
  return "protocol-refusal";
}

/** An upload id rotates only after the server explicitly rejects that id. */
export function uploadFailureMayRotate(cause: RecoveryCause): boolean {
  return cause === "unknown-upload-id";
}
