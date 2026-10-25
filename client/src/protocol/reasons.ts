// Names the eight reasons any failure carries (api/failurereasons.txt), with the words every view shows for them.
export const FAILURE_REASONS = {
  "path-failed": "Path unavailable",
  "connection-lost": "Connection lost",
  timeout: "Stopped delivering data",
  "sign-in-required": "Sign-in required",
  "client-limit": "Limit reached for your address or sign-in",
  "server-capacity": "Server at capacity",
  "protocol-error": "Unexpected server response",
  "insufficient-evidence": "Too little measured time",
} as const;

export type FailureReason = keyof typeof FAILURE_REASONS;

/** Reasons earlier builds saved under the name they had then. */
const RENAMED = new Map<string, FailureReason>([
  ["preparation-failed", "path-failed"],
  ["server-busy", "server-capacity"],
]);

/** A saved reason by its current name; null for one this build does not know, as a newer build may save. */
export function knownReason(saved: string): FailureReason | null {
  const reason = RENAMED.get(saved) ?? saved;
  return Object.hasOwn(FAILURE_REASONS, reason) ? (reason as FailureReason) : null;
}
