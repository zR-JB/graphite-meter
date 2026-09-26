import { MISSING } from "../presentation/vocabulary";
import {
  fmtMs,
  fmtSpeed,
  rateScaleIndex,
  rateUnit,
  rateValueAt,
} from "../format";
import type { StageStatus } from "./types";

interface HistoryUnits {
  base: "base10" | "base2";
  kind: "bits" | "bytes";
}

export function formatHistoryRate(
  bytesPerSec: number | null | undefined,
  units: HistoryUnits,
): string {
  if (bytesPerSec == null) return MISSING;
  const baseUnits = units.kind === "bits" ? bytesPerSec * 8 : bytesPerSec;
  const tier = rateScaleIndex(baseUnits, units.base);
  return `${fmtSpeed(rateValueAt(bytesPerSec, units.base, units.kind, tier))} ${rateUnit(units.base, units.kind, tier)}`;
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

export function formatPercent(
  value: number | null | undefined,
  fractionDigits = 1,
): string {
  if (value == null) return MISSING;
  return `${value.toFixed(Number.isInteger(value) ? 0 : fractionDigits)}%`;
}

export function stageStatusLabel(status: StageStatus): string {
  return status === "not-run"
    ? "Skipped"
    : status === "failed"
      ? "Failed"
      : status === "partial"
        ? "Partial"
        : MISSING;
}
