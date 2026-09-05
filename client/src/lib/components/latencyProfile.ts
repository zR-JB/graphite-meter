// Pure geometry, formatting, and hover-selection logic behind LatencyProfile.svelte.
import { fmtMs, niceDomain } from "../format";
import type { ReflectorTimingSummary, TransportRole } from "../runner/contract";

export const LATENCY_LANES = [
  { key: "latency", label: "Idle" },
  { key: "download", label: "Loaded Down" },
  { key: "upload", label: "Loaded Up" },
  { key: "bidirectional", label: "Loaded Bi-dir" },
] as const;

export type MetricKey = "min" | "p10" | "center" | "p90" | "max" | "current";

const METRIC_ORDER: readonly MetricKey[] = [
  "min",
  "p10",
  "center",
  "p90",
  "max",
  "current",
];

const METRIC_LABELS: Record<Exclude<MetricKey, "center">, string> = {
  min: "Min",
  p10: "P10",
  p90: "P90",
  max: "Max",
  current: "Latest",
};

// The chart's value range; min is the left edge, span its width in the metric's own units (niceDomain's {min, span}).
export interface LatencyProfileDomain {
  min: number;
  max: number;
  span: number;
}

type LatencyProfileLaneLike = {
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  center: number | null;
  current?: number | null;
  centerKind?: "average" | "result";
};

export type LatencyProfileTone =
  "latency" | "download" | "upload" | "bidirectional";

export interface LatencyProfileViewLane extends LatencyProfileLaneLike {
  reflectorTiming?: ReflectorTimingSummary;
  key: TransportRole;
  label: string;
  tone: LatencyProfileTone;
  jitter: number | null;
  timeoutRatio: number | null;
  accountingComplete: boolean | null;
  timeoutCount: number | null;
  unresolvedCount: number | null;
  sendFailureCount: number | null;
  count: number;
  active?: boolean;
}

/** Shared value-domain policy for live and finalized latency profiles. */
export function profileDomain(
  lanes: readonly LatencyProfileLaneLike[],
): LatencyProfileDomain {
  const values = lanes.flatMap((lane) =>
    [lane.min, lane.max].filter((value): value is number => value != null),
  );
  return niceDomain(values, { floor: 1 });
}

// Position of a value as a 0 to 100% offset along the track, clamped at both ends.
export function pos(
  value: number | null,
  domain: LatencyProfileDomain,
): number {
  if (value == null) return 0;
  return Math.min(100, Math.max(0, ((value - domain.min) / domain.span) * 100));
}

// Exact interval width as a percentage. Fixed caps keep a flat range visible.
export function rangeWidth(
  min: number | null,
  max: number | null,
  domain: LatencyProfileDomain,
): number {
  if (min == null || max == null) return 0;
  return Math.max(0, pos(max, domain) - pos(min, domain));
}

export function tickLabel(v: number): string {
  return v <= 0 ? "0" : fmtMs(v);
}

// Sub-1% timeouts keeps a second decimal so a rare drop is still legible.
export function timeoutLabel(ratio: number): string {
  if (ratio <= 0) return "";
  return `${(ratio * 100).toFixed(ratio < 0.01 ? 2 : 1)}% timeouts`;
}

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

export function metricLabel(
  lane: LatencyProfileLaneLike,
  metric: MetricKey,
): string {
  if (metric === "center")
    return lane.centerKind === "result" ? "Median" : "Mean";
  return METRIC_LABELS[metric];
}

function centerLabel(lane: LatencyProfileLaneLike): string {
  return lane.center == null
    ? ""
    : `${metricLabel(lane, "center")} ${fmtMs(lane.center)}`;
}

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

// Secondary line under the hovered metric: the band it belongs to, or the lane's center as a fallback anchor.
export function hoverContext(
  lane: LatencyProfileLaneLike,
  metric: MetricKey,
): string {
  if (metric === "p10" || metric === "p90") {
    if (lane.p10 == null || lane.p90 == null) return "";
    return `P10–P90 ${fmtMs(lane.p10)} – ${fmtMs(lane.p90)}`;
  }
  if (metric === "current") {
    return centerLabel(lane);
  }
  if (metric === "center") {
    if (lane.min == null || lane.max == null) return "";
    return `Range ${fmtMs(lane.min)} – ${fmtMs(lane.max)}`;
  }
  return centerLabel(lane);
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
    `${lane.count} resolved`,
    lane.timeoutCount == null ? null : `${lane.timeoutCount} timeouts`,
    lane.unresolvedCount == null ? null : `${lane.unresolvedCount} unresolved`,
    lane.sendFailureCount == null
      ? null
      : `${lane.sendFailureCount} send failures`,
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
        ? `${count} resolved`
        : `${replied} ${replied === 1 ? "reply" : "replies"}`,
    exceptions: [
      (lane.timeoutCount ?? 0) > 0
        ? `${lane.timeoutCount} ${lane.timeoutCount === 1 ? "timeout" : "timeouts"}`
        : null,
      (lane.unresolvedCount ?? 0) > 0
        ? `${lane.unresolvedCount} unresolved`
        : null,
      (lane.sendFailureCount ?? 0) > 0
        ? `${lane.sendFailureCount} ${lane.sendFailureCount === 1 ? "send failure" : "send failures"}`
        : null,
    ].filter((value): value is string => value !== null),
  };
}

export function reflectorTimingDescription(
  timing: ReflectorTimingSummary,
): string {
  return `Server timing · ${timing.sampleCount} paired replies
Mean RTT: ${fmtMs(timing.meanRawRttMs)} ms raw − ${fmtMs(timing.meanHandlingMs)} ms server handling = ${fmtMs(timing.meanAdjustedRttMs)} ms adjusted.`;
}
