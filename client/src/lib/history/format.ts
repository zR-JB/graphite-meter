import { MISSING, type Outcome } from "../presentation/vocabulary";
import {
  fmtMs,
  fmtSpeed,
  rateScaleIndex,
  rateUnit,
  rateValueAt,
} from "../format";
import type { HistoryRecord, StageStatus } from "./types";

interface HistoryUnits {
  base: "base10" | "base2";
  kind: "bits" | "bytes";
}

/** Saved rates choose their own unit tier; a live run's tier never applies. */
export function historyRate(bytesPerSec: number, units: HistoryUnits) {
  const baseUnits = units.kind === "bits" ? bytesPerSec * 8 : bytesPerSec;
  const tier = rateScaleIndex(baseUnits, units.base);
  return {
    num: fmtSpeed(rateValueAt(bytesPerSec, units.base, units.kind, tier)),
    unit: rateUnit(units.base, units.kind, tier),
  };
}

export function formatHistoryRate(
  bytesPerSec: number | null | undefined,
  units: HistoryUnits,
): string {
  if (bytesPerSec == null) return MISSING;
  const { num, unit } = historyRate(bytesPerSec, units);
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

export function stageStatusLabel(status: StageStatus): string {
  return status === "not-run"
    ? "Skipped"
    : status === "failed"
      ? "Failed"
      : status === "partial"
        ? "Partial"
        : MISSING;
}

/** One completeness rule for the list badge and the detail. */
export function historyOutcome(record: HistoryRecord): Outcome {
  if (record.outcome === "incomplete") return "incomplete";
  const { latency, download, upload, bidirectional } = record.stages;
  return record.outcome === "partial" ||
    record.failures.length > 0 ||
    (record.multiServer?.failures.length ?? 0) > 0 ||
    [latency, download, upload, bidirectional].some(
      ({ status }) => status === "partial" || status === "failed",
    )
    ? "partial"
    : "complete";
}
