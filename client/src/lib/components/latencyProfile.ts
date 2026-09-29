// Pure geometry, formatting, and hover-selection logic behind LatencyProfile.svelte.
import { fmtCount, fmtMs } from "../format";
import { latencyScale } from "../presentation/scales";
import type { ReflectorTimingSummary, TransportRole } from "../runner/contract";
import { LATENCY_POPULATION, MISSING } from "../presentation/vocabulary";
import { STAGES } from "../runner/schedule";

export const LATENCY_LANES = STAGES.map((key) => ({
  key,
  label: LATENCY_POPULATION[key].short,
}));

export type MetricKey =
  "min" | "p10" | "center" | "p90" | "p95" | "max" | "current";

const METRIC_ORDER: readonly MetricKey[] = [
  "min",
  "p10",
  "center",
  "p90",
  "p95",
  "max",
  "current",
];

const METRICS: Record<MetricKey, { label: string; meaning: string }> = {
  min: { label: "Min", meaning: "Fastest reply" },
  p10: { label: "P10", meaning: "10% of replies at or below" },
  center: { label: "Median", meaning: "Half of the replies were faster" },
  p90: { label: "P90", meaning: "90% of replies at or below" },
  p95: { label: "P95", meaning: "95% of replies at or below" },
  max: { label: "Max", meaning: "Slowest reply" },
  current: { label: "Latest", meaning: "Median of the latest replies" },
};

type LatencyProfileLaneLike = {
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  p95?: number | null;
  center: number | null;
  current?: number | null;
};

export interface LatencyProfileViewLane extends LatencyProfileLaneLike {
  reflectorTiming?: ReflectorTimingSummary;
  key: TransportRole;
  label: string;
  jitter: number | null;
  timeoutRatio: number | null;
  accountingComplete: boolean | null;
  timeoutCount: number | null;
  unresolvedCount: number | null;
  sendFailureCount: number | null;
  count: number;
  active?: boolean;
  /** Why this population's probes stopped, by server when several ran. */
  failure?: string;
}

/** The lanes' axis follows the gauge's rule over their P90s, so the boxes fill it; a slower reply runs off its end. */
export const profileDomain = (lanes: readonly LatencyProfileLaneLike[]) =>
  latencyScale(lanes.map((lane) => lane.p90 ?? lane.center));

// Position of a value as a 0 to 100% offset along the track, clamped at both ends.
export function pos(value: number | null, maxMs: number): number {
  if (value == null) return 0;
  return Math.min(100, Math.max(0, (value / maxMs) * 100));
}

// The share of resolved probes that timed out; sub-1% keeps a second decimal so a rare timeout is still legible.
export const formatTimeouts = (ratio: number | null) =>
  ratio == null
    ? MISSING
    : `${(ratio * 100).toFixed(ratio > 0 && ratio < 0.01 ? 2 : 1)}%`;

/** Both supported latency transports provide application probe timeout evidence. */
export function savedLatencyHasProbeEvidence(kind: string | null): boolean {
  return kind === "webtransport" || kind === "websocket";
}

export function metricValue(
  lane: LatencyProfileLaneLike,
  metric: MetricKey,
): number | null {
  return lane[metric] ?? null;
}

export const metricLabel = (metric: MetricKey) => METRICS[metric].label;
export const metricMeaning = (metric: MetricKey) => METRICS[metric].meaning;

// The present metrics in label order, dropping any the lane has not measured.
export function entries(
  lane: LatencyProfileLaneLike,
): { metric: MetricKey; value: number }[] {
  return METRIC_ORDER.flatMap((metric) => {
    const value = metricValue(lane, metric);
    return value == null ? [] : [{ metric, value }];
  });
}

// The measured metric whose value sits closest to a hovered position.
export function nearestMetric(
  lane: LatencyProfileLaneLike,
  target: number,
): MetricKey | null {
  return entries(lane).reduce<MetricKey | null>((best, entry) => {
    if (!best) return entry.metric;
    const bestValue = metricValue(lane, best)!;
    return Math.abs(entry.value - target) < Math.abs(bestValue - target)
      ? entry.metric
      : best;
  }, null);
}

export const PARTIAL_ACCOUNTING_HELP =
  "Some probe outcomes are unknown. Counts cover known outcomes only.";

export function probeAccountingDetails(
  lane: Pick<
    LatencyProfileViewLane,
    | "count"
    | "timeoutCount"
    | "unresolvedCount"
    | "sendFailureCount"
    | "accountingComplete"
  >,
): string {
  const counts = [
    `${fmtCount(lane.count)} resolved`,
    lane.timeoutCount == null
      ? null
      : `${fmtCount(lane.timeoutCount)} timeouts`,
    lane.unresolvedCount == null
      ? null
      : `${fmtCount(lane.unresolvedCount)} unresolved`,
    lane.sendFailureCount == null
      ? null
      : `${fmtCount(lane.sendFailureCount)} send failures`,
  ]
    .filter((value): value is string => value !== null)
    .join(" · ");
  return lane.accountingComplete === false
    ? `Known: ${counts}. Additional outcomes unknown.`
    : counts;
}

export function hasProbeAccountingNotice(
  lane: Pick<
    LatencyProfileViewLane,
    | "accountingComplete"
    | "timeoutCount"
    | "unresolvedCount"
    | "sendFailureCount"
  >,
): boolean {
  return (
    lane.accountingComplete === false ||
    (lane.timeoutCount ?? 0) > 0 ||
    (lane.unresolvedCount ?? 0) > 0 ||
    (lane.sendFailureCount ?? 0) > 0
  );
}

/** Compact visible counts; complete accounting stays available to assistive technology. */
export function probeAccountingSummary(
  lane: Pick<
    LatencyProfileViewLane,
    "count" | "timeoutCount" | "unresolvedCount" | "sendFailureCount"
  >,
): { replies: string; exceptions: string[] } {
  const count = lane.count;
  const replied = lane.timeoutCount == null ? null : count - lane.timeoutCount;
  return {
    replies:
      replied == null
        ? `${fmtCount(count)} resolved`
        : `${fmtCount(replied)} ${replied === 1 ? "reply" : "replies"}`,
    exceptions: [
      (lane.timeoutCount ?? 0) > 0
        ? `${fmtCount(lane.timeoutCount ?? 0)} ${lane.timeoutCount === 1 ? "timeout" : "timeouts"}`
        : null,
      (lane.unresolvedCount ?? 0) > 0
        ? `${fmtCount(lane.unresolvedCount ?? 0)} unresolved`
        : null,
      (lane.sendFailureCount ?? 0) > 0
        ? `${fmtCount(lane.sendFailureCount ?? 0)} ${lane.sendFailureCount === 1 ? "send failure" : "send failures"}`
        : null,
    ].filter((value): value is string => value !== null),
  };
}

const NOT_LOSS = "A timeout is a missing reply, not packet loss";

export const probeOutcomes = (
  lane: Parameters<typeof probeAccountingDetails>[0] & { label: string },
) =>
  `${lane.label} probe outcomes\n${probeAccountingDetails(lane)}\n${NOT_LOSS}`;

/** What a timeouts share counts; nothing to explain before any probe resolved. */
export const timeoutsTip = (
  lane: Pick<LatencyProfileViewLane, "count" | "timeoutCount">,
) =>
  lane.count
    ? `Timeouts\n${fmtCount(lane.timeoutCount ?? 0)} of ${fmtCount(lane.count)} probes had no reply before the deadline\n${NOT_LOSS}`
    : "";

export const serverHandling = (timing: ReflectorTimingSummary) =>
  `Server handling ${fmtMs(timing.meanHandlingMs)} ms of a ${fmtMs(timing.meanRawRttMs)} ms ` +
  `mean round trip (${fmtCount(timing.sampleCount)} paired replies)`;
