import type { ConnectivityState, LatencyBucket } from "../runner/contract";

type HealthBucket = Pick<
  LatencyBucket,
  "startT" | "endT" | "pingCount" | "timeoutCount" | "medianRttMs"
>;

const STALE_MS = 3_000;
const WINDOW_MS = 4_000;

/** The probes of the last 4 s of evidence: what the indicator judges and states. */
export function recentProbes(buckets: readonly HealthBucket[]) {
  const latest = buckets.at(-1);
  const recent = latest
    ? buckets
        .slice(
          buckets.findLastIndex(
            (bucket) => bucket.endT < latest.endT - WINDOW_MS,
          ) + 1,
        )
        .filter((bucket) => bucket.pingCount > 0)
    : [];
  return {
    recent,
    replies: recent.flatMap((bucket) =>
      bucket.medianRttMs === null ? [] : [bucket.medianRttMs],
    ),
    probes: recent.reduce((sum, bucket) => sum + bucket.pingCount, 0),
    timeouts: recent.reduce((sum, bucket) => sum + bucket.timeoutCount, 0),
  };
}

/**
 * A live indicator from missing replies only, not the run's timeout statistic; evidence older than 3 s at `nowT`
 * is stale. Latency is a value to show, never a verdict: a loaded stage raises it by design.
 */
export function connectionQuality(
  buckets: readonly HealthBucket[],
  nowT = -Infinity,
): ConnectivityState | "checking" {
  const latest = buckets.at(-1);
  if (!latest || nowT - latest.endT > STALE_MS) return "checking";
  const { recent, probes, timeouts } = recentProbes(buckets);

  // A clean tail of replies and elapsed time supersedes old timeouts at any cadence.
  let cleanReplies = 0;
  for (let i = recent.length - 1; i >= 0; i--) {
    const bucket = recent[i];
    if (bucket.timeoutCount || bucket.medianRttMs === null) break;
    cleanReplies += bucket.pingCount;
    if (
      cleanReplies >= 2 &&
      i < recent.length - 1 &&
      latest.endT - bucket.startT >= 800
    )
      return "connected";
  }

  // One timeout is too little evidence, above all at the sparse idle cadence.
  if (timeouts >= 2 && timeouts / probes >= 0.2) return "unstable";
  if (timeouts >= 2 && timeouts / probes >= 0.02) return "degraded";
  return "connected";
}
