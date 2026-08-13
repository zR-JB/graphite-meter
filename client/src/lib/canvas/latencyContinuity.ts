import type { LatencyBucket } from "../runner/contract";

/** Largest truthful gap between adjacent latency buckets that may be drawn as
 * one continuity segment. Larger gaps remain visually and interactively empty. */
export const LATENCY_CONTINUITY_GAP_MS = 600;

/** Two buckets share a rendered segment only when each contains an observed
 * RTT and their authoritative continuity and timing agree. A partial-loss
 * bucket can still join the line; an all-loss bucket cannot invent an RTT. */
export function latencyBucketsContinuous(
  left: LatencyBucket,
  right: LatencyBucket,
): boolean {
  return (
    left.medianRttMs != null &&
    right.medianRttMs != null &&
    left.phase === right.phase &&
    left.underLoad === right.underLoad &&
    left.continuityId === right.continuityId &&
    right.startT - left.endT <= LATENCY_CONTINUITY_GAP_MS
  );
}

/** Finds a real bucket nearest to `t`, but only if the pointer is inside that
 * rendered continuity segment. It deliberately never interpolates RTT. */
export function nearestLatencyBucketInContinuity(
  buckets: readonly LatencyBucket[],
  t: number,
): LatencyBucket | null {
  let start = 0;
  for (let end = 1; end <= buckets.length; end++) {
    if (
      end !== buckets.length &&
      latencyBucketsContinuous(buckets[end - 1], buckets[end])
    ) {
      continue;
    }
    const first = buckets[start];
    const last = buckets[end - 1];
    if (first && last && t >= first.startT && t <= last.endT) {
      let nearest = first;
      for (let i = start + 1; i < end; i++) {
        const candidate = buckets[i];
        if (Math.abs(candidate.t - t) < Math.abs(nearest.t - t))
          nearest = candidate;
      }
      return nearest;
    }
    start = end;
  }
  return null;
}

/** P95 adds useful context only when it differs enough from median latency to
 * survive display rounding and convey tail behavior. */
export function materiallyDifferentP95(
  medianRttMs: number | null,
  p95RttMs: number | null,
): boolean {
  return (
    medianRttMs != null &&
    p95RttMs != null &&
    p95RttMs - medianRttMs >= Math.max(2, medianRttMs * 0.15)
  );
}
