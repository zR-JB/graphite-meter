const ERROR_TEXT =
  /failed to fetch|networkerror|domexception|\b[a-z]*error\b|\bhttp\s+\d{3}\b/i;
const MECHANISM =
  /\b(?:fetch|probe|request|route|transport|webtransport|websocket|stream|worker)\b/i;

/* Replace known low-level network details at user-facing measurement seams. */
export function failureDetail(
  detail: string | undefined,
  fallback = "Connection lost",
): string {
  if (!detail) return fallback;
  return ERROR_TEXT.test(detail) || MECHANISM.test(detail) ? fallback : detail;
}
