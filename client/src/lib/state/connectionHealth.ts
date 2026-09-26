import type { ConnectivityState, LatencyBucket } from "../runner/contract";
import { median } from "../runner/measure";

type HealthBucket = Pick<
  LatencyBucket,
  "startT" | "endT" | "pingCount" | "timeoutCount" | "medianRttMs"
>;

/** A live indicator, not the run's timeout or jitter statistic. */
export function connectionQuality(
  buckets: readonly HealthBucket[],
): ConnectivityState {
  const latest = buckets.at(-1);
  if (!latest) return "connected";
  const recent = buckets.filter(
    (bucket) => bucket.endT >= latest.endT - 4000 && bucket.pingCount > 0,
  );
  const replies = recent.flatMap((bucket) =>
    bucket.medianRttMs === null ? [] : [bucket.medianRttMs],
  );
  const variationThreshold = Math.max(20, median(replies) * 0.3);

  // A sustained clean tail supersedes an old spike or timeout burst. Requiring
  // elapsed evidence as well as replies makes recovery independent of cadence.
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

  const timeouts = recent.reduce((sum, bucket) => sum + bucket.timeoutCount, 0);
  const count = recent.reduce((sum, bucket) => sum + bucket.pingCount, 0);
  // One timeout is insufficient evidence for a quality warning, especially
  // on the sparse idle cadence where it otherwise means 25–100% timeouts.
  if (timeouts >= 2 && timeouts / count >= 0.2) return "unstable";
  if (timeouts >= 2 && timeouts / count >= 0.02) return "degraded";
  const changes = replies.slice(1).map((rtt, i) => Math.abs(rtt - replies[i]));
  // An isolated spike produces two large changes; require repeated variation.
  if (
    changes.filter((change) => change > variationThreshold).length >= 3 &&
    median(changes) > variationThreshold
  )
    return "degraded";
  return "connected";
}
