// The latency lanes' pure rules: the axis, the metrics a row is read at, and the words that describe a row.
import type { LatencySummary } from "../../engine/measure/latency";
import { fmtAddedMs, fmtCount, fmtMs, fmtPercent, MISSING } from "../../view/format";
import { latencyCeilingMs } from "../../view/scales";

export type Metric = "min" | "p10" | "center" | "p90" | "p95" | "max" | "current";

const METRICS: readonly Metric[] = ["min", "p10", "center", "p90", "p95", "max", "current"];

const NAMES: Record<Metric, { label: string; meaning: string }> = {
  min: { label: "Min", meaning: "fastest reply" },
  p10: { label: "P10", meaning: "10% of replies at or below" },
  center: { label: "Median", meaning: "half of the replies were faster" },
  p90: { label: "P90", meaning: "90% of replies at or below" },
  p95: { label: "P95", meaning: "95% of replies at or below" },
  max: { label: "Max", meaning: "slowest reply" },
  current: { label: "Latest", meaning: "median of the latest replies" },
};
export const metricLabel = (metric: Metric) => NAMES[metric].label;
export const metricMeaning = (metric: Metric) => NAMES[metric].meaning;

/** A row's values in ms, null where unmeasured. */
export type Values = Record<Metric, number | null>;

export function values(summary: LatencySummary | null, current: number | null = null): Values {
  return {
    min: summary?.minMs ?? null,
    p10: summary?.p10Ms ?? null,
    center: summary?.p50Ms ?? null,
    p90: summary?.p90Ms ?? null,
    p95: summary?.p95Ms ?? null,
    max: summary?.maxMs ?? null,
    current,
  };
}

/** The drawn metrics a row can be read at, along the axis; P95 has no mark, so it is spoken, never pointed at. */
export const present = (row: Values): Metric[] =>
  METRICS.filter((metric) => metric !== "p95" && row[metric] !== null).sort((a, b) => row[a]! - row[b]!);

/** The rows' axis follows the dial's ladder over their P90s, so the boxes fill it and one slow reply never sets it. */
export const lanesAxisMs = (rows: readonly Values[]) => latencyCeilingMs(rows.map((row) => row.p90 ?? row.center));

/** The measured metric nearest a value on the axis, so a hand follows from marker to marker; null with none. */
export function nearestMetric(row: Values, ms: number): Metric | null {
  let best: Metric | null = null;
  for (const metric of present(row))
    if (best === null || Math.abs(row[metric]! - ms) < Math.abs(row[best]! - ms)) best = metric;
  return best;
}

/** A slider key's step through the present metrics, null for any other key; from no reading, a step down takes the
    last. */
export function stepMetric(row: Values, from: Metric | null, key: string): Metric | null {
  const metrics = present(row);
  const index = from === null ? -1 : metrics.indexOf(from);
  const last = metrics.length - 1;
  const up = key === "ArrowRight" || key === "ArrowUp";
  const down = key === "ArrowLeft" || key === "ArrowDown";
  const next =
    key === "Home"
      ? 0
      : key === "End"
        ? last
        : up
          ? Math.min(last, index + 1)
          : down
            ? index < 0
              ? last
              : Math.max(0, index - 1)
            : -1;
  return metrics[next] ?? null;
}

/** A twentieth of the probes timing out is worth a look. */
export const TIMEOUTS_WARN = 0.05;

/** The share of resolved probes that timed out; null before any resolved. */
export const timeoutShare = (summary: LatencySummary | null) =>
  summary?.probeCount ? summary.timeoutCount / summary.probeCount : null;

export const fmtTimeouts = (ratio: number | null) => (ratio === null ? MISSING : fmtPercent(ratio * 100));

/** Unknown outcomes or anything but a reply: the figure says it explains them. */
export const accountingNotice = (summary: LatencySummary | null) =>
  !!summary &&
  (!summary.accountingComplete ||
    summary.timeoutCount > 0 ||
    summary.unresolvedCount > 0 ||
    summary.sendFailureCount > 0);

const NOT_LOSS = "A timeout is a missing reply, not packet loss";

function accounting(summary: LatencySummary): string {
  const counts = [
    `${fmtCount(summary.probeCount)} resolved`,
    `${fmtCount(summary.timeoutCount)} timeouts`,
    `${fmtCount(summary.unresolvedCount)} unresolved`,
    `${fmtCount(summary.sendFailureCount)} send failures`,
  ].join(", ");
  return summary.accountingComplete ? counts : `Known: ${counts}. Additional outcomes unknown.`;
}

/** The figure's explainer: its outcomes when it has a notice, else what the share counts. */
export function timeoutsTip(label: string, summary: LatencySummary | null): string {
  if (!summary?.probeCount && !accountingNotice(summary)) return "";
  if (accountingNotice(summary)) return `${label} probe outcomes\n${accounting(summary!)}\n${NOT_LOSS}`;
  return `Timeouts\n${fmtCount(summary!.timeoutCount)} of ${fmtCount(summary!.probeCount)} probes had no reply before the deadline\n${NOT_LOSS}`;
}

/** The server's share of the round trip, from replies that carried it. */
export const serverHandling = (timing: NonNullable<LatencySummary["reflectorTiming"]>) =>
  `Server handling ${fmtMs(timing.meanHandlingMs)} ms of a ${fmtMs(timing.meanRawRttMs)} ms mean round trip ` +
  `(${fmtCount(timing.sampleCount)} paired replies)`;

/** A row as a screen reader hears it: every figure it has, or that it waits. */
export function laneDescription(label: string, row: Values, summary: LatencySummary | null, added: number | null) {
  const ms = (value: number) => `${fmtMs(value)} ms`;
  const ratio = timeoutShare(summary);
  const figures = [
    row.center === null ? null : `median ${ms(row.center)}`,
    row.min === null || row.max === null ? null : `range ${fmtMs(row.min)} to ${ms(row.max)}`,
    row.p10 === null || row.p90 === null ? null : `P10 to P90 ${fmtMs(row.p10)} to ${ms(row.p90)}`,
    row.p95 === null ? null : `P95 ${ms(row.p95)}`,
    summary?.jitterMs == null ? null : `jitter ${ms(summary.jitterMs)}`,
    added === null ? null : `added over idle ${fmtAddedMs(added)} ms`,
    ratio === null ? null : `${fmtTimeouts(ratio)} probe timeouts`,
  ].filter((part) => part !== null);
  if (!figures.length) return `${label} latency profile: waiting for measurements`;
  return `${label} latency profile: ${figures.join(", ")}${summary ? `. ${accounting(summary)}` : ""}`;
}
