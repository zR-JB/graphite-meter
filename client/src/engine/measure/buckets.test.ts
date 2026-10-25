import { expect, test } from "bun:test";
import { bucketWidth, LatencyBuckets, type LatencyBucket } from "./buckets";

function buckets(phase: LatencyBucket["phase"] & string = "latency", start = 0, continuity = 0) {
  const emitted: LatencyBucket[] = [];
  const presented = new LatencyBuckets((bucket) => emitted.push(bucket));
  if (phase !== "idle") presented.reset(start, phase, continuity, null);
  return { presented, emitted };
}

test("a timeout counts as a probe, never as an RTT, and a timeout-only bucket has none", () => {
  const { presented, emitted } = buckets("download", 0, 3);
  presented.observe(10, 20, false);
  presented.observe(20, 9_999, true);
  presented.closeThrough(200);
  expect(emitted).toEqual([
    expect.objectContaining({
      phase: "download",
      continuityId: 3,
      pingCount: 2,
      timeoutCount: 1,
      medianRttMs: 20,
    }),
  ]);
  presented.observe(205, 0, true);
  presented.flush(300);
  expect(emitted[1]).toMatchObject({
    startT: 200,
    endT: 300,
    pingCount: 1,
    timeoutCount: 1,
    medianRttMs: null,
  });
});

test("a bucket closes at its end without a later probe, and late outcomes revise the bucket that owns their time once per tick", () => {
  const { presented, emitted } = buckets();
  presented.observe(50, 10, false);
  presented.closeThrough(199);
  expect(emitted).toHaveLength(0);
  presented.closeThrough(200);
  expect(emitted).toHaveLength(1);
  expect(presented.nextEnd).toBe(400);
  presented.observe(250, 10, false);
  presented.observe(150, 30, false);
  presented.observe(160, 40, false);
  expect(emitted).toHaveLength(1);
  presented.closeThrough(260);
  expect(emitted).toHaveLength(2);
  expect(emitted[1]).toMatchObject({ startT: 0, pingCount: 3, maxRttMs: 40 });
});

test("a long page suspension skips empty time without inventing outcomes and keeps recent revisions", () => {
  const day = 24 * 60 * 60 * 1000;
  for (const intervalMs of [null, 600])
    for (const back of [day + 73, day + 12]) {
      const emitted: LatencyBucket[] = [];
      const presented = new LatencyBuckets((bucket) => emitted.push(bucket));
      presented.reset(13, "download", 7, intervalMs);
      const width = bucketWidth(intervalMs);
      presented.observe(50, 11, false);
      presented.closeThrough(back);
      expect(emitted).toHaveLength(1);
      // The open bucket covers the time it closed through.
      expect(presented.nextEnd! - width).toBeLessThanOrEqual(back);
      expect(presented.nextEnd!).toBeGreaterThan(back);
      // Outcomes from before the horizon, the probe's own first bucket included, are gone.
      presented.observe(back - 10_400 - width, 90, false);
      presented.observe(60, 99, false);
      expect(emitted).toHaveLength(1);
      presented.observe(back - 800, 23, false);
      presented.flush(back);
      expect(emitted.at(-1)).toMatchObject({ pingCount: 1, medianRttMs: 23, continuityId: 7 });
      expect(emitted.slice(1).every(({ maxRttMs }) => maxRttMs === 23)).toBe(true);
    }
});
