// Pure geometry, formatting, and hover-selection logic behind
// LatencyProfile.svelte. Positions are relative to the chart's value domain,
// passed in so this module never touches the rune store.
import { fmtMs } from "../format";
import type { LatencyLane } from "../state/store.svelte";

export type MetricKey = "min" | "p10" | "average" | "p90" | "max" | "current";

export const METRIC_LABELS: Record<MetricKey, string> = {
  min: "Min",
  p10: "P10",
  average: "Avg",
  p90: "P90",
  max: "Max",
  current: "Latest",
};

// The chart's value range; min is the left edge, span its width in the metric's
// own units (niceDomain's {min, span}).
export interface Domain {
  min: number;
  span: number;
}

// Position of a value as a 0 to 100% offset along the track, clamped at both
// ends.
export function pos(value: number | null, domain: Domain): number {
  if (value == null) return 0;
  return Math.min(100, Math.max(0, ((value - domain.min) / domain.span) * 100));
}

// Width of a min/max band as a percentage, never thinner than a hairline so a
// flat distribution still shows.
export function rangeWidth(
  min: number | null,
  max: number | null,
  domain: Domain,
): number {
  if (min == null || max == null) return 0;
  return Math.max(1.5, pos(max, domain) - pos(min, domain));
}

export function tickLabel(v: number): string {
  return v <= 0 ? "0" : fmtMs(v);
}

// Sub-1% loss keeps a second decimal so a rare drop is still legible.
export function lossLabel(ratio: number): string {
  if (ratio <= 0) return "";
  return `${(ratio * 100).toFixed(ratio < 0.01 ? 2 : 1)}% loss`;
}

export function metricValue(
  lane: LatencyLane,
  metric: MetricKey,
): number | null {
  return lane[metric];
}

// The present metrics in label order, dropping any the lane has not measured.
export function entries(
  lane: LatencyLane,
): { metric: MetricKey; value: number }[] {
  return (Object.keys(METRIC_LABELS) as MetricKey[]).flatMap((metric) => {
    const value = metricValue(lane, metric);
    return value == null ? [] : [{ metric, value }];
  });
}

// The measured metric whose value sits closest to a hovered position.
export function nearestMetric(
  lane: LatencyLane,
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

// Secondary line under the hovered metric: the band it belongs to, or the
// lane's average as a fallback anchor.
export function hoverContext(lane: LatencyLane, metric: MetricKey): string {
  if (metric === "p10" || metric === "p90") {
    if (lane.p10 == null || lane.p90 == null) return "";
    return `P10–P90 ${fmtMs(lane.p10)} – ${fmtMs(lane.p90)}`;
  }
  if (metric === "current") {
    return lane.average == null ? "" : `Avg ${fmtMs(lane.average)}`;
  }
  if (metric === "average") {
    if (lane.min == null || lane.max == null) return "";
    return `Range ${fmtMs(lane.min)} – ${fmtMs(lane.max)}`;
  }
  return lane.average == null ? "" : `Avg ${fmtMs(lane.average)}`;
}
