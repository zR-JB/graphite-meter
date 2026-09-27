import type { ConnectivityState, LatencyBucket } from "../runner/contract";
import { median } from "../runner/measure";

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

/** A live indicator, not the run's timeout or jitter statistic; evidence older than 3 s at `nowT` is stale. */
export function connectionQuality(
  buckets: readonly HealthBucket[],
  nowT = -Infinity,
): ConnectivityState | "checking" {
  const latest = buckets.at(-1);
  if (!latest || nowT - latest.endT > STALE_MS) return "checking";
  const { recent, replies, probes, timeouts } = recentProbes(buckets);
  const variationThreshold = Math.max(20, median(replies) * 0.3);

  // A clean tail of replies and elapsed time supersedes an old spike at any cadence.
  let cleanReplies = 0;
  for (let i = recent.length - 1; i >= 0; i--) {
    const bucket = recent[i];
    if (
      bucket.timeoutCount ||
      bucket.medianRttMs === null ||
      latest.medianRttMs === null ||
      Math.abs(bucket.medianRttMs - latest.medianRttMs) > variationThreshold
    )
      break;
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
  const changes = replies.slice(1).map((rtt, i) => Math.abs(rtt - replies[i]));
  // An isolated spike produces two large changes; require repeated variation.
  if (
    changes.filter((change) => change > variationThreshold).length >= 3 &&
    median(changes) > variationThreshold
  )
    return "degraded";
  return "connected";
}
