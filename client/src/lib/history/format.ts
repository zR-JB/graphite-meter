import { MISSING } from "../presentation/vocabulary";
import { fmtMs, resultRate } from "../format";

export function formatHistoryRate(
  bytesPerSec: number | null | undefined,
  units: Parameters<typeof resultRate>[1],
): string {
  if (bytesPerSec == null) return MISSING;
  const { num, unit } = resultRate(bytesPerSec, units);
  return `${num} ${unit}`;
}

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

export function formatLatency(value: number | null | undefined): string {
  return value == null ? MISSING : `${fmtMs(value)} ms`;
}
