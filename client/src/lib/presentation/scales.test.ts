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

test("a rate sets the throughput axes only once it has held for 700 ms", () => {
  const spike = ticks(0, [1e6, 1e6, 1e8, 1e6, 1e6, 1e6, 1e6, 1e6, 1e6, 1e6]);
  expect(scales(spike).chartBytesPerSec).toBe(1.25e6);
  const held = ticks(0, Array(9).fill(5e6));
  expect(scales(held.slice(0, 7)).chartBytesPerSec).toBe(12.5e6);
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
  // From the mega tier up, the gauge keeps a 1 Gbit/s floor.
  expect(scales(held).gaugeBytesPerSec).toBe(125e6);
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
