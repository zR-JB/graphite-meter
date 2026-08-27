/* Replace known low-level network details at user-facing measurement seams. */
export function failureDetail(
  detail: string | undefined,
  fallback = "Connection lost",
): string {
  if (!detail) return fallback;
  return /failed to fetch|networkerror|domexception|\b[a-z]+error\b|\berror\b|\bhttp\s+\d{3}\b|\b(?:fetch|probe|request|route|transport|webtransport|websocket|stream|worker)\b/i.test(
    detail,
  )
    ? fallback
    : detail;
}
