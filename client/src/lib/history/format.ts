const RELATIVE_TIME_LIMIT_MS = 60 * 60 * 1_000;

export function formatRecentCompletion(
  completedAt: number,
  now = Date.now(),
): string | null {
  const elapsed = Math.max(0, now - completedAt);
  if (elapsed >= RELATIVE_TIME_LIMIT_MS) return null;
  if (elapsed < 60_000) return "now";
  const minutes = Math.floor(elapsed / 60_000);
  return `${minutes} min ago`;
}
