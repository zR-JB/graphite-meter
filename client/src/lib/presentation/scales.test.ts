import { expect, test } from "bun:test";
import type { ThroughputSample } from "../runner/contract";
import { singleLatencyBucket } from "../runner/series";
import {
  gaugeLatency,
  latencyAxisMs,
  latencyBucketExceedsScale,
  latencyScale,
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

test("each throughput axis contains what it draws, on a 1-2-5 step with headroom", () => {
  const gigabit = ticks(0, [117e6, 125.4e6, 118e6]);
  // One sample just over 1 Gbit/s steps both axes to 2 Gbit/s, never a decade.
  expect(scales(gigabit)).toEqual({
    chartBytesPerSec: 250e6,
    gaugeBytesPerSec: 250e6,
    unitIndex: 2,
  });
  expect(scales(ticks(0, [117e6])).chartBytesPerSec).toBe(125e6);
  // The chart draws each bidirectional lane; the gauge their sum.
  const bidirectional = ticks(0, Array(3).fill(50e6)).flatMap((sample) => [
    sample,
    { ...sample, dir: "up" as const },
  ]);
  expect(scales(bidirectional).chartBytesPerSec).toBe(62.5e6);
  expect(scales(bidirectional).gaugeBytesPerSec).toBe(125e6);
  const download = {
    peakBytesPerSec: null,
    stabilityPct: 100,
    totalBytes: 0,
    reportedBytesPerSec: 6e6,
  };
  expect(scales([], { ...NO_RESULT, download }).chartBytesPerSec).toBe(6.25e6);
  expect(throughputScales([], NO_RESULT, 2e6, "base10", "bits")).toEqual({
    chartBytesPerSec: 2e6,
    gaugeBytesPerSec: 2e6,
    unitIndex: 2,
  });
});

test("latency axes follow the p95 on the tier ladder, live over 8 s", () => {
  expect(latencyScale([])).toBe(20);
  expect(latencyScale([0.1])).toBe(1);
  expect(latencyScale([3])).toBe(4);
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
