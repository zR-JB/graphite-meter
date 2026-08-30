import {
  fmtBytes,
  fmtMs,
  fmtSpeed,
  rateScaleIndex,
  rateUnit,
  rateValueAt,
} from "../format";
import type { StageStatus } from "./types";

export interface HistoryUnits {
  base: "base10" | "base2";
  kind: "bits" | "bytes";
}

export function formatHistoryRate(
  bytesPerSec: number | null | undefined,
  units: HistoryUnits,
): string {
  if (bytesPerSec == null) return "Unavailable";
  const baseUnits = units.kind === "bits" ? bytesPerSec * 8 : bytesPerSec;
  const tier = rateScaleIndex(baseUnits, units.base);
  return `${fmtSpeed(rateValueAt(bytesPerSec, units.base, units.kind, tier))} ${rateUnit(units.base, units.kind, tier)}`;
}

export function formatHistoryBytes(
  bytes: number,
  base: HistoryUnits["base"],
): string {
  return fmtBytes(bytes, base);
}

export function formatDuration(durationMs: number): string {
  const ms = Math.max(0, durationMs);
  if (ms < 1_000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) {
    const seconds = ms / 1_000;
    return `${seconds < 10 ? seconds.toFixed(1) : seconds.toFixed(0)} s`;
  }
  const totalSeconds = Math.round(ms / 1_000);
  if (totalSeconds < 3_600) {
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return seconds ? `${minutes} min ${seconds} s` : `${minutes} min`;
  }
  const totalMinutes = Math.round(ms / 60_000);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  return minutes ? `${hours} h ${minutes} min` : `${hours} h`;
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
  return value == null ? "Unavailable" : `${fmtMs(value)} ms`;
}

export function formatPercent(
  value: number | null | undefined,
  fractionDigits = 1,
): string {
  if (value == null) return "Unavailable";
  return `${value.toFixed(Number.isInteger(value) ? 0 : fractionDigits)}%`;
}

export function stageStatusLabel(status: StageStatus): string {
  switch (status) {
    case "complete":
      return "Measured";
    case "partial":
      return "Partial";
    case "failed":
      return "Unavailable";
    case "not-run":
      return "Not run";
  }
}
