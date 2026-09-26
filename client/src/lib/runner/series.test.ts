import { describe, expect, test } from "bun:test";
import type { ThroughputSample } from "./contract";
import {
  compactThroughputHistory,
  LatencyPresentationBuckets,
  LatencyScaleController,
  latencyBucketExceedsScale,
  latencyBucketMs,
  latencyScale,
  singleLatencyBucket,
  upsertLatencyBucket,
} from "./series";

const rate = (t: number, bytesPerSec: number): ThroughputSample => ({
  t,
  bytesPerSec,
  bytesCumulative: 0,
  dir: "down",
  phase: "download",
  continuityId: 0,
});

describe("latency presentation buckets", () => {
  test("a timeout counts as a ping but never as an RTT point", () => {
    const buckets = new LatencyPresentationBuckets();
    buckets.reset(0, "download", true, 3);
    buckets.observe(10, 20, false);
    buckets.observe(20, 9_999, true);
    const [bucket] = buckets.closeThrough(200);
    expect(bucket).toMatchObject({
      phase: "download",
      underLoad: true,
      continuityId: 3,
      pingCount: 2,
      timeoutCount: 1,
      medianRttMs: 20,
      maxRttMs: 20,
    });
  });

  test("a timeout-only bucket has no RTT", () => {
    const buckets = new LatencyPresentationBuckets();
    buckets.reset(0, "latency", false, 0);
    buckets.observe(5, 0, true);
    expect(buckets.flush()).toMatchObject({
      pingCount: 1,
      timeoutCount: 1,
      medianRttMs: null,
    });
  });

  test("the deadline closes a bucket without a later ping", () => {
    const buckets = new LatencyPresentationBuckets();
    buckets.reset(0, "latency", false, 0);
    buckets.observe(50, 12, false);
    expect(buckets.closeThrough(199)).toEqual([]);
    expect(buckets.closeThrough(200)).toHaveLength(1);
    expect(buckets.nextBoundaryT).toBe(400);
  });

  test("a late outcome revises its closed bucket", () => {
    const buckets = new LatencyPresentationBuckets();
    buckets.reset(0, "latency", false, 0);
    buckets.observe(50, 10, false);
    buckets.observe(250, 10, false);
    const [revised] = buckets.observe(150, 30, false);
    expect(revised).toMatchObject({ startT: 0, pingCount: 2, maxRttMs: 30 });
  });

  test("fixed-cadence widths are whole ping intervals", () => {
    expect(latencyBucketMs(0, 150)).toBe(300);
    expect(latencyBucketMs(600_000)).toBe(600);
  });
});

describe("latency history", () => {
  test("a revision reports a structural change; a tail append does not", () => {
    const history = [singleLatencyBucket(0, 10, false)];
    expect(
      upsertLatencyBucket(history, singleLatencyBucket(10, 12, false)),
    ).toBe(false);
    expect(
      upsertLatencyBucket(history, singleLatencyBucket(10, 14, false)),
    ).toBe(true);
    expect(upsertLatencyBucket(history, singleLatencyBucket(5, 8, false))).toBe(
      true,
    );
    expect(history.map((bucket) => bucket.medianRttMs)).toEqual([10, 8, 14]);
  });

  test("compaction keeps the success-weighted median and the worst tail", () => {
    const history = [10, 90, 20, 40, 50].map((rtt, t) =>
      singleLatencyBucket(t, rtt, false),
    );
    history.push(singleLatencyBucket(4.5, 0, true));
    upsertLatencyBucket(history, singleLatencyBucket(5, 60, false), 2);
    expect(history).toHaveLength(2);
    expect(history[0]).toMatchObject({
      medianRttMs: 40,
      maxRttMs: 90,
      pingCount: 6,
      timeoutCount: 1,
    });
  });
});

describe("throughput history", () => {
  test("compaction keeps the peak, the dip, and both ends", () => {
    const history = Array.from({ length: 100 }, (_, i) => rate(i * 10, 100));
    history[40] = rate(400, 900);
    history[60] = rate(600, 1);
    expect(compactThroughputHistory(history, 0, 20)).toBe(true);
    expect(history.length).toBeLessThanOrEqual(20);
    const values = history.map((sample) => sample.bytesPerSec);
    expect(values).toContain(900);
    expect(values).toContain(1);
    expect(history[0].t).toBe(0);
    expect(history.at(-1)!.t).toBe(990);
  });
});

describe("latency scale", () => {
  test("follows the p95 of medians with headroom on the tier ladder", () => {
    expect(latencyScale([])).toBe(20);
    expect(latencyScale([null, 30])).toBe(40);
    expect(latencyScale([5_000])).toBe(10_000);
  });

  test("the maximum can exceed the scale that its median sets", () => {
    const bucket = { ...singleLatencyBucket(0, 10, false), maxRttMs: 25 };
    expect(latencyBucketExceedsScale(bucket, latencyScale([10]))).toBe(true);
  });

  test("grows at once and shrinks one tier after a dwell", () => {
    const scale = new LatencyScaleController();
    const at = (t: number, rtt: number) =>
      scale.observe({ ...singleLatencyBucket(t, rtt, false), endT: t });
    expect(at(0, 100)).toBe(200);
    expect(at(6_100, 10)).toBe(200);
    expect(at(7_000, 10)).toBe(200);
    expect(at(8_200, 10)).toBe(100);
  });
});
