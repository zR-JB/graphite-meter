import type { LatencyBucket } from "../runner/contract";
interface LatencyGlyphHover {
  bucket: LatencyBucket;
  x: number;
}
interface LatencyOverflowGlyph {
  arrow: {
    tipY: number;
    baseY: number;
    halfWidth: number;
  };
  dot: { y: number; radius: number };
}
/* CSS-pixel composition for a clipped latency observation. */
export function latencyOverflowGlyph(plotTop: number): LatencyOverflowGlyph {
  const radius = 2.25;
  const tipY = plotTop + 1.5;
  const baseY = tipY + 5;
  return {
    arrow: { tipY, baseY, halfWidth: 3.5 },
    dot: { y: baseY + 2 + radius, radius },
  };
}
/* Selects only a real latency or loss glyph near the pointer. */
export function nearestLatencyGlyph(
  lanes: Iterable<readonly LatencyBucket[]>,
  pointerX: number,
  xForTime: (t: number) => number,
  radiusPx = 10,
): LatencyGlyphHover | null {
  let nearest: LatencyGlyphHover | null = null;
  let nearestDistance = Infinity;
  for (const lane of lanes) {
    let lo = 0;
    let hi = lane.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (xForTime(lane[mid].t) < pointerX) lo = mid + 1;
      else hi = mid;
    }
    // Only the closest visible glyph on either side can win. Empty buckets
    // carry no invented RTT, and looking up a long run does not scan its history.
    for (const direction of [-1, 1]) {
      for (
        let index = direction < 0 ? lo - 1 : lo;
        index >= 0 && index < lane.length;
        index += direction
      ) {
        const bucket = lane[index];
        const x = xForTime(bucket.t);
        const distance = Math.abs(x - pointerX);
        if (distance > radiusPx || distance >= nearestDistance) break;
        if (bucket.medianRttMs == null && bucket.lossCount === 0) continue;
        nearest = { bucket, x };
        nearestDistance = distance;
        break;
      }
    }
  }
  return nearest;
}
