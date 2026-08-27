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
