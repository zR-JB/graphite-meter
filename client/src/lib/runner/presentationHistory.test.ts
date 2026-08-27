import { expect, test } from "bun:test";
import type { LatencyBucket, ThroughputSample } from "./contract";
import {
  appendThroughputSample,
  compactLatencyHistory,
  compactThroughputHistory,
} from "./presentationHistory";
test("long throughput history stays bounded across the complete timeline", () => {
  const history: ThroughputSample[] = [];
  for (let i = 0; i < 20_000; i++) {
    appendThroughputSample(
      history,
      {
        t: i * 100,
        bytesPerSec: i === 9_999 ? 99_999 : 1_000 + (i % 23),
        bytesCumulative: i * 100,
        dir: "down",
        phase: "download",
        continuityId: i < 10_000 ? 1 : 2,
      },
      120,
    );
  }
  expect(history.length).toBeLessThanOrEqual(120);
  expect(history[0].t).toBe(0);
  expect(history.at(-1)?.t).toBe(1_999_900);
  expect(history.some((sample) => sample.bytesPerSec === 99_999)).toBe(true);
  expect(new Set(history.map((sample) => sample.continuityId))).toEqual(
    new Set([1, 2]),
  );
});
test("throughput rebuild keeps real lane extrema and phase boundaries", () => {
  const history: ThroughputSample[] = [];
  const add = (
    t: number,
    phase: ThroughputSample["phase"],
    dir: ThroughputSample["dir"],
    bytesPerSec: number,
    continuityId: number,
  ) =>
    history.push({
      t,
      phase,
      dir,
      bytesPerSec,
      bytesCumulative: t,
      continuityId,
    });
  for (let i = 0; i < 100; i++) add(i * 10, "download", "down", i, 1);
  for (let i = 0; i < 100; i++) {
    add(1_000 + i * 10, "bidirectional", "down", i === 40 ? 900 : i, 2);
    add(1_000 + i * 10, "bidirectional", "up", i === 60 ? 800 : i, 2);
  }
  for (let i = 0; i < 100; i++) add(2_000 + i * 10, "upload", "up", i, 3);
  expect(compactThroughputHistory(history, 4_000_000, 40)).toBe(true);
  expect(history.length).toBeLessThanOrEqual(40);
  expect(history.every((sample) => Number.isInteger(sample.bytesPerSec))).toBe(
    true,
  );
  expect(history.some((sample) => sample.bytesPerSec === 900)).toBe(true);
  expect(history.some((sample) => sample.bytesPerSec === 800)).toBe(true);
  expect(new Set(history.map((sample) => sample.phase))).toEqual(
    new Set(["download", "bidirectional", "upload"]),
  );
  expect(new Set(history.map((sample) => sample.continuityId))).toEqual(
    new Set([1, 2, 3]),
  );
  expect(history[0].t).toBe(0);
  expect(history.at(-1)?.t).toBe(2_990);
});
test("latency compaction preserves totals, endpoints, maximum, and jitter", () => {
  const history: LatencyBucket[] = Array.from({ length: 400 }, (_, i) => ({
    t: i * 200 + 100,
    startT: i * 200,
    endT: (i + 1) * 200,
    medianRttMs: 10 + (i % 3),
    p95RttMs: 12 + (i % 5),
    maxRttMs: i === 200 ? 500 : 15,
    firstRttMs: 10 + (i % 3),
    lastRttMs: 10 + (i % 3),
    rttDeltaSumMs: 2,
    rttDeltaCount: 1,
    pingCount: 4,
    lossCount: i % 10 === 0 ? 1 : 0,
    underLoad: false,
    phase: "latency",
    continuityId: 1,
  }));
  const pings = history.reduce((sum, sample) => sum + sample.pingCount, 0);
  const losses = history.reduce((sum, sample) => sum + sample.lossCount, 0);
  compactLatencyHistory(history, 40);
  expect(history.length).toBeLessThanOrEqual(40);
  expect(history[0].startT).toBe(0);
  expect(history.at(-1)?.endT).toBe(80_000);
  expect(history.reduce((sum, sample) => sum + sample.pingCount, 0)).toBe(
    pings,
  );
  expect(history.reduce((sum, sample) => sum + sample.lossCount, 0)).toBe(
    losses,
  );
  expect(Math.max(...history.map((sample) => sample.maxRttMs ?? 0))).toBe(500);
});
