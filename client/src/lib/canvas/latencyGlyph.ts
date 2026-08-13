import type { LatencyBucket } from "../runner/contract";

export interface LatencyGlyphHover {
  bucket: LatencyBucket;
  x: number;
}

/** Selects only a real latency or loss glyph near the pointer. Unlike a line
 * chart, sparse RTT buckets never imply a value between observations. */
export function nearestLatencyGlyph(
  lanes: Iterable<readonly LatencyBucket[]>,
  pointerX: number,
  xForTime: (t: number) => number,
  radiusPx = 10,
): LatencyGlyphHover | null {
  let nearest: LatencyGlyphHover | null = null;
  let nearestDistance = Infinity;
  for (const lane of lanes) {
    for (const bucket of lane) {
      if (bucket.medianRttMs == null && bucket.lossCount === 0) continue;
      const x = xForTime(bucket.t);
      const distance = Math.abs(x - pointerX);
      if (distance > radiusPx || distance >= nearestDistance) continue;
      nearest = { bucket, x };
      nearestDistance = distance;
    }
  }
  return nearest;
}
