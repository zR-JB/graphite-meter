import { expect, test } from "bun:test";
import type { ThroughputSample } from "../runner/contract";
import { singleLatencyBucket } from "../runner/series";
import {
  gaugeLatency,
  gaugeScaleForPeak,
  latencyAxisMs,
  latencyBucketExceedsScale,
  latencyScale,
  niceDomain,
  throughputScales,
} from "./scales";

const NO_RESULT: Parameters<typeof throughputScales>[1] = {
  download: null,
  upload: null,
  bidirectional: null,
};
const ticks = (from: number, rates: number[]): ThroughputSample[] =>
  rates.map((bytesPerSec, i) => ({
    t: from + i * 100,
    bytesPerSec,
    bytesCumulative: 0,
    dir: "down",
    phase: "download",
    continuityId: 0,
  }));
const scales = (series: ThroughputSample[], result = NO_RESULT) =>
  throughputScales(series, result, "auto", "base10", "bits");

test("a brief spike never sets the throughput axes; a first sample does", () => {
  const spike = ticks(0, [1e6, 1e6, 2e8, 1e6, 1e6, 1e6, 1e6, 1e6, 1e6, 1e6]);
  expect(scales(spike).chartBytesPerSec).toBe(1.25e6);
  const held = ticks(0, Array(9).fill(5e6));
  // Before 700 ms exist the lowest rate so far sets them, so a first sample never sits off the scale.
  expect(scales(held.slice(0, 1)).gaugeBytesPerSec).toBe(125e6);
  expect(scales(held.slice(0, 1)).chartBytesPerSec).toBe(6.25e6);
  expect(scales(held).chartBytesPerSec).toBe(6.25e6);
  const download = {
    peakBytesPerSec: null,
    stabilityPct: 100,
    totalBytes: 0,
    reportedBytesPerSec: 6e6,
  };
  expect(scales([], { ...NO_RESULT, download }).chartBytesPerSec).toBe(6.25e6);
  const bidirectional = ticks(0, Array(9).fill(3e6)).flatMap((sample) => [
    sample,
    { ...sample, dir: "up" as const },
  ]);
  expect(scales(bidirectional).chartBytesPerSec).toBe(6.25e6);
  // From the mega tier up, the gauge keeps a 1 Gbit/s floor, and no rate ever sits off its scale.
  expect(scales(held).gaugeBytesPerSec).toBe(125e6);
  expect(scales(spike).gaugeBytesPerSec).toBe(1.25e9);
  expect(throughputScales(spike, NO_RESULT, 2e6, "base10", "bits")).toEqual({
    chartBytesPerSec: 2e6,
    gaugeBytesPerSec: 12.5e6,
    unitIndex: 2,
  });
});

test("latency axes follow the p95 on the tier ladder, live over 8 s", () => {
  expect(latencyScale([])).toBe(20);
  expect(latencyScale([null, 30])).toBe(40);
  expect(latencyScale([5_000])).toBe(10_000);
  const history = [...Array(10).fill(100), ...Array(90).fill(10)].map(
    (rtt, i) => ({
      ...singleLatencyBucket(i * 100, rtt, false, "latency"),
      endT: i * 100,
    }),
  );
  expect(latencyAxisMs(history.slice(0, 80), false)).toBe(200);
  expect(latencyAxisMs(history, false)).toBe(20);
  expect(latencyAxisMs(history, true)).toBe(200);
  const spike = { ...singleLatencyBucket(0, 10, false), maxRttMs: 25 };
  expect(latencyBucketExceedsScale(spike, latencyScale([10]))).toBe(true);
});

test("the gauge uses the shared axis only once a bucket measured the RTT it shows", () => {
  const measured = [singleLatencyBucket(0, 25, false, "latency")];
  const timeout = [singleLatencyBucket(0, 0, true, "latency")];
  for (const [phase, history, completedRttMs, expected] of [
    ["latency", [], null, { rttMs: 600, scaleMs: 1_000 }],
    ["latency", measured, null, { rttMs: 600, scaleMs: 40 }],
    ["complete", measured, 30, { rttMs: 30, scaleMs: 40 }],
    ["complete", timeout, 600, { rttMs: 600, scaleMs: 1_000 }],
  ] as const)
    expect(
      gaugeLatency({
        phase,
        liveRttMs: 600,
        axisMs: 40,
        history,
        completedRttMs,
      }),
    ).toEqual(expected);
});

test("the gauge takes the next decade above its floor; the chart keeps its 1-2-5 step", () => {
  expect(gaugeScaleForPeak(1, 1e9)).toBe(125_000_000);
  expect(gaugeScaleForPeak(125_000_001, 1e9)).toBe(1_250_000_000);
  expect(gaugeScaleForPeak(12_501)).toBe(125_000);
  expect(gaugeScaleForPeak(12_500_000)).toBe(12_500_000);
});

test("chart domains snap to a 1-2-5 ladder without collapsing a flat range", () => {
  const ranges = [[], [10, 12], [100, 900], [50, 50]];
  expect(ranges.map((values) => niceDomain(values))).toEqual([
    { min: 0, max: 12, span: 12 },
    { min: 0, max: 20, span: 20 },
    { min: 0, max: 2000, span: 2000 },
    { min: 40, max: 60, span: 20 },
  ]);
});
