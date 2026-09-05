import { expect, test } from "bun:test";
import type { LatencyBucket } from "../runner/contract";
import { latencyOverflowGlyph, nearestLatencyGlyph } from "./latencyGlyph";

function bucket(
  t: number,
  medianRttMs: number | null,
  lossCount = 0,
): LatencyBucket {
  return {
    t,
    startT: t - 100,
    endT: t,
    phase: "latency",
    continuityId: 1,
    underLoad: false,
    medianRttMs,
    p95RttMs: medianRttMs,
    maxRttMs: medianRttMs,
    firstRttMs: medianRttMs,
    lastRttMs: medianRttMs,
    pingCount: medianRttMs == null ? lossCount : 1 + lossCount,
    lossCount,
    rttDeltaSumMs: 0,
    rttDeltaCount: 0,
  };
}

test("latency hover selects only a nearby real bucket glyph", () => {
  const early = bucket(100, 20);
  const late = bucket(900, 40);
  const x = (t: number) => t / 10;

  expect(nearestLatencyGlyph([[early, late]], 11, x)?.bucket).toBe(early);
  // A continuous phase does not create a hoverable implied RTT midway.
  expect(nearestLatencyGlyph([[early, late]], 50, x)).toBeNull();
});

test("a nearby loss glyph remains hoverable without a fabricated RTT", () => {
  const loss = bucket(400, null, 3);
  const hit = nearestLatencyGlyph([[loss]], 40, (t) => t / 10);
  expect(hit?.bucket).toBe(loss);
  expect(hit?.bucket.medianRttMs).toBeNull();
});

test("an overflow arrow and its clipped dot retain a visible CSS-pixel gap", () => {
  const glyph = latencyOverflowGlyph(12);
  expect(glyph.arrow.tipY).toBeGreaterThan(12);
  expect(glyph.arrow.baseY).toBeGreaterThan(glyph.arrow.tipY);
  expect(
    glyph.dot.y - glyph.dot.radius - glyph.arrow.baseY,
  ).toBeGreaterThanOrEqual(2);
});

test("latency inspection stays bounded with a long high-cadence history", () => {
  const history = Array.from({ length: 20_000 }, (_, index) =>
    bucket(index, 20),
  );
  let lookups = 0;
  const selected = nearestLatencyGlyph([history], 9_999.2, (t) => {
    lookups++;
    return t;
  });
  expect(selected?.bucket).toBe(history[9_999]);
  expect(lookups).toBeLessThan(32);
});

test("empty buckets do not hide the nearest measured glyph", () => {
  const measured = bucket(20, 12);
  const empty = bucket(21, null);
  expect(nearestLatencyGlyph([[measured, empty]], 21, (t) => t)?.bucket).toBe(
    measured,
  );
});
