// Names the eight reasons any failure carries (api/failurereasons.txt), with the words every view shows for them.
export const FAILURE_REASONS = {
  "path-failed": "Path unavailable",
  "connection-lost": "Connection lost",
  timeout: "Stopped delivering data",
  "sign-in-required": "Sign-in required",
  "client-limit": "Limit reached for your address or sign-in",
  "server-busy": "Server at capacity",
  "protocol-error": "Unexpected server response",
  "insufficient-evidence": "Too little measured time",
} as const;

export type FailureReason = keyof typeof FAILURE_REASONS;
