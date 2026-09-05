import { test, expect } from "bun:test";
import {
  MIN_PARTIAL_LATENCY_OUTCOMES,
  MIN_PARTIAL_TRANSFER_EVIDENCE_MS,
  RunAccumulator,
} from "./evaluation";
import type { AdaptiveDurationConfig } from "./contract";
import { DEFAULT_CONFIG } from "../state/defaults";
const adaptive: AdaptiveDurationConfig = {
  enabled: true,
  minCoverageRatio: 0,
  stabilityThreshold: 0.9,
  maxPhaseReductionRatio: 1,
  minLatencySamples: 0,
  minTransferSamples: 0,
  confirmationMs: 100,
};
function push(accum: RunAccumulator, value: number): void {
  accum.pushThroughput("download", "down", value, 1);
  const conf = accum.confidence("download");
  accum.trackStableRun("download", conf.score, adaptive);
}
function mean(values: number[]): number {
  return values.reduce((s, v) => s + v, 0) / values.length;
}
function fresh(): RunAccumulator {
  const accum = new RunAccumulator();
  accum.reset();
  return accum;
}
function adaptiveTrace(finalValues: readonly number[]): {
  accum: RunAccumulator;
  samples: number[];
} {
  const accum = fresh();
  const samples: number[] = [];
  const add = (values: readonly number[]) => {
    samples.push(...values);
    for (const value of values) push(accum, value);
  };
  add(Array.from({ length: 60 }, (_, i) => 100 + i * (800 / 59)));
  add(Array(90).fill(1000));
  add(Array.from({ length: 20 }, (_, i) => (i % 2 === 0 ? 100 : 2000)));
  add(finalValues);
  return { accum, samples };
}
function pushBidi(
  accum: RunAccumulator,
  direction: "down" | "up",
  value: number,
  score?: number,
  duration = 1,
  authoritative = false,
  observedAtMs?: number,
): void {
  accum.pushThroughput(
    "bidirectional",
    direction,
    value * duration,
    duration,
    authoritative,
    observedAtMs,
  );
  if (score !== undefined)
    accum.trackStableRun("bidirectional", score, adaptive);
}
function pushEqualBidi(
  accum: RunAccumulator,
  direction: "down" | "up",
  values: readonly number[],
  score?: number,
  duration = 1,
  authoritative = false,
): void {
  for (const value of values)
    pushBidi(accum, direction, value, score, duration, authoritative);
}
function pushBidiPairs(
  accum: RunAccumulator,
  downValues: readonly number[],
  upValue: number,
  upAuthoritative = false,
): void {
  for (const downValue of downValues) {
    pushBidi(accum, "down", downValue);
    pushBidi(accum, "up", upValue, undefined, 1, upAuthoritative);
  }
}
function descriptiveStability(
  rates: readonly number[],
  chunkMs: readonly number[],
): number {
  const accum = new RunAccumulator();
  accum.reset();
  let traceIndex = 0;
  for (const durationMs of chunkMs) {
    const rate = rates[traceIndex % rates.length];
    accum.pushThroughput(
      "download",
      "down",
      (rate * durationMs) / 1_000,
      durationMs / 1_000,
    );
    traceIndex++;
  }
  return accum.throughputResult("download", false).stabilityPct;
}
test("descriptive stability is invariant to callback chunking", () => {
  const rates = [950, 1050, 950, 1050, 1050, 950, 1050, 950];
  const wholeBuckets = Array(rates.length).fill(250);
  const whole = descriptiveStability(rates, wholeBuckets);
  const split = new RunAccumulator();
  split.reset();
  for (const rate of rates) {
    for (let i = 0; i < 5; i++)
      split.pushThroughput("download", "down", rate * 0.05, 0.05);
  }
  expect(whole).toBeCloseTo(95, 8);
  expect(split.throughputResult("download", false).stabilityPct).toBeCloseTo(
    whole,
    8,
  );
});
test.each([
  [5, [950, 1050, 950, 1050, 1050, 950, 1050, 950]],
  [15, [850, 1150, 850, 1150, 1150, 850, 1150, 850]],
  [30, [700, 1300, 700, 1300, 1300, 700, 1300, 700]],
])("descriptive stability reports %i percent CV", (cv, rates) => {
  expect(
    descriptiveStability(rates, Array(rates.length).fill(250)),
  ).toBeCloseTo(100 - cv, 5);
});

test.each([
  [
    "adaptive throughput reports the final plateau after stability recovers",
    Array(100).fill(3000),
    true,
    "stable-window",
    3000,
  ],
  [
    "an unstable ending falls back to the entire measurement phase",
    [],
    true,
    "full-average",
    undefined,
  ],
  [
    "adaptive completion off always reports the entire measurement phase",
    Array(60).fill(1000),
    false,
    "full-average",
    undefined,
  ],
])("%s", (_label, finalValues, useAdaptive, method, expected) => {
  const trace = useAdaptive
    ? adaptiveTrace(finalValues)
    : (() => {
        const accum = fresh();
        const samples = [
          ...Array.from({ length: 60 }, (_, i) => 100 + i * (800 / 59)),
          ...finalValues,
        ];
        for (const value of samples) push(accum, value);
        return { accum, samples };
      })();
  const result = trace.accum.throughputResult("download", useAdaptive);
  expect(result.method).toBe(method as typeof result.method);
  expect(result.fullAverageBytesPerSec).toBeCloseTo(mean(trace.samples), 6);
  expect(result.meanBytesPerSec).toBeCloseTo(
    expected ?? result.fullAverageBytesPerSec,
    6,
  );
  if (expected !== undefined)
    expect(result.meanBytesPerSec).not.toBeCloseTo(1000, 0);
});
test("transfer headline weights samples by represented time", () => {
  const accum = fresh();
  accum.pushThroughput("upload", "up", 10, 0.1);
  accum.pushThroughput("upload", "up", 10, 1);
  const result = accum.throughputResult("upload", false);
  expect(result.fullAverageBytesPerSec).toBeCloseTo(20 / 1.1, 6);
  expect(result.fullAverageBytesPerSec).not.toBeCloseTo(55, 6);
});
test("the final stable plateau is also weighted by represented time", () => {
  const accum = fresh();
  accum.pushThroughput("download", "down", 100, 0.1);
  accum.trackStableRun("download", 0, adaptive);
  accum.pushThroughput("download", "down", 100, 1);
  accum.trackStableRun("download", 1, adaptive);
  const result = accum.throughputResult("download", true);
  expect(result.method).toBe("stable-window");
  expect(result.meanBytesPerSec).toBeCloseTo(100, 6);
});
test("stability first established on the final one-way observation remains reducible", () => {
  const accum = fresh();
  accum.pushThroughput("upload", "up", 1000, 1);
  accum.trackStableRun("upload", 0, adaptive);
  accum.pushThroughput("upload", "up", 500, 2, true);
  accum.trackStableRun("upload", 1, adaptive);
  const result = accum.throughputResult("upload", true);
  expect(result.method).toBe("stable-window");
  expect(result.meanBytesPerSec).toBeCloseTo(250, 6);
  expect(result.fullAverageBytesPerSec).toBeCloseTo(500, 6);
  expect(result.serverAuthoritative).toBe(true);
});
test("bidirectional: interleaved arrival order doesn't cross-contaminate the lanes", () => {
  const accum = fresh();
  const downs = [400, 420, 440, 460];
  const ups = [100, 120, 140, 160];
  for (let i = 0; i < downs.length; i++) {
    accum.pushThroughput("bidirectional", "up", ups[i], 1, true);
    accum.pushThroughput("bidirectional", "down", downs[i], 1);
  }
  const result = accum.bidirectionalResult(false);
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(mean(downs), 6);
  expect(result.up.fullAverageBytesPerSec).toBeCloseTo(mean(ups), 6);
  expect(result.down.serverAuthoritative).toBeUndefined();
  expect(result.up.serverAuthoritative).toBe(true);
  expect(result.down.totalBytes).toBe(1720);
  expect(result.up.totalBytes).toBe(520);
});
test("bidirectional confidence keeps an uneven trailing window aligned", () => {
  const accum = fresh();
  for (let i = 0; i < 32; i++)
    accum.pushThroughput("bidirectional", "down", i * 0.25, 0.25);
  for (let i = 0; i < 24; i++)
    accum.pushThroughput("bidirectional", "up", (100 + i) * 0.25, 0.25);
  expect(accum.confidence("bidirectional").sampleCount).toBe(8);
});
test("bidirectional confidence rejects non-overlapping restarted lane windows", () => {
  const accum = fresh();
  for (let i = 0; i < 40; i++)
    pushBidi(accum, "down", 1_000, undefined, 0.25, false, (i + 1) * 250);
  for (let i = 0; i < 16; i++)
    pushBidi(accum, "up", 2_000, undefined, 0.25, false, (24 + i + 1) * 250);
  expect(accum.confidence("bidirectional").sampleCount).toBe(16);
  pushBidi(accum, "up", 2_000, undefined, 0.25, false, 80 * 250 + 250);
  expect(accum.confidence("bidirectional").sampleCount).toBe(0);
});
test("bidirectional: one lane still empty (staggered start) reports the other correctly", () => {
  const accum = fresh();
  pushEqualBidi(accum, "down", Array(10).fill(700));
  const result = accum.bidirectionalResult(false);
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(700, 6);
  expect(result.up.fullAverageBytesPerSec).toBe(0);
  expect(result.up.totalBytes).toBe(0);
});
test("bidirectional final plateau aligns each interleaved lane to shared stability", () => {
  const accum = fresh();
  pushBidi(accum, "down", 100, 1);
  pushBidi(accum, "up", 50, 1);
  pushBidi(accum, "down", 100, 1);
  pushBidi(accum, "up", 50, 1);
  pushBidi(accum, "down", 10, 0);
  pushBidi(accum, "up", 5, 0);
  pushBidi(accum, "down", 500, 0);
  pushBidi(accum, "up", 300, 1);
  pushBidi(accum, "down", 500, 1);
  pushBidi(accum, "up", 300, 1);
  const result = accum.bidirectionalResult(true);
  expect(result.down.method).toBe("stable-window");
  expect(result.up.method).toBe("stable-window");
  expect(result.down.meanBytesPerSec).toBeCloseTo(500, 6);
  expect(result.up.meanBytesPerSec).toBeCloseTo(300, 6);
});
test("bidirectional stable evidence also requires actual early completion", () => {
  const accum = fresh();
  for (let i = 0; i < 4; i++) {
    pushBidi(accum, "down", 100, 0);
    pushBidi(accum, "up", 50, 0);
  }
  for (let i = 0; i < 4; i++) {
    pushBidi(accum, "down", 1000, 1);
    pushBidi(accum, "up", 500, 1);
  }
  const result = accum.bidirectionalResult(false);
  expect(result.down.method).toBe("full-average");
  expect(result.up.method).toBe("full-average");
  expect(result.down.reportedBytesPerSec).toBeCloseTo(550, 6);
  expect(result.up.reportedBytesPerSec).toBeCloseTo(275, 6);
});
test("final bidirectional observation preserves the opening evidence in both lanes", () => {
  const accum = fresh();
  pushBidi(accum, "down", 100, 0);
  pushBidi(accum, "up", 50, 0);
  pushBidi(accum, "down", 500, 0);
  pushBidi(accum, "up", 300, 1);
  const result = accum.bidirectionalResult(true);
  expect(result.down.method).toBe("stable-window");
  expect(result.up.method).toBe("stable-window");
  expect(result.down.meanBytesPerSec).toBeCloseTo(500, 6);
  expect(result.up.meanBytesPerSec).toBeCloseTo(300, 6);
});
test("bidirectional: shared stability degrades when either lane alone turns erratic", () => {
  const stable = fresh();
  pushBidiPairs(stable, Array(40).fill(500), 300);
  const stableScore = stable.confidence("bidirectional").score;
  const erratic = fresh();
  pushBidiPairs(
    erratic,
    Array.from({ length: 40 }, (_, i) => (i % 2 === 0 ? 100 : 900)),
    300,
  );
  const erraticScore = erratic.confidence("bidirectional").score;
  expect(stableScore).toBeGreaterThan(erraticScore);
});
test("partial transfer keeps whole exact evidence only after its named floor", () => {
  const accum = fresh();
  accum.pushThroughput(
    "download",
    "down",
    700,
    (MIN_PARTIAL_TRANSFER_EVIDENCE_MS - 100) / 1_000,
  );
  expect(accum.partialThroughputResult("download")).toBeNull();
  accum.pushThroughput("download", "down", 100, 0.1);
  const result = accum.partialThroughputResult("download");
  expect(result?.method).toBe("full-average");
  expect(result?.totalBytes).toBe(800);
  expect(result?.reportedBytesPerSec).toBeCloseTo(1_000, 6);
});
test("a recovery gap reaches only the final byte/time reduction", () => {
  const accum = fresh();
  accum.pushThroughput("upload", "up", 1_000, 1, true);
  const controlSamples = accum.confidence("upload").sampleCount;
  accum.recordRecoveryGap("upload", "up", 1);
  const result = accum.partialThroughputResult("upload");
  expect(result?.totalBytes).toBe(1_000);
  expect(result?.fullAverageBytesPerSec).toBe(500);
  expect(accum.confidence("upload").sampleCount).toBe(controlSamples);
});
test("replacement checkpoint bytes enter only the exact final reduction", () => {
  const accum = fresh();
  accum.recordRecoveryGap("upload", "up", 1);
  accum.recordRecoveryBytes("upload", "up", 1_000);
  const result = accum.partialThroughputResult("upload");
  expect(result).not.toBeNull();
  expect(result?.totalBytes).toBe(1_000);
  expect(result?.fullAverageBytesPerSec).toBe(1_000);
  expect(result?.serverAuthoritative).toBe(true);
});
test("partial latency needs named outcome and success evidence floors", () => {
  const accum = fresh();
  for (let i = 0; i < MIN_PARTIAL_LATENCY_OUTCOMES - 1; i++)
    accum.pushLatency("latency", 20, i > 0, i * 100);
  expect(accum.partialLatencyResult(DEFAULT_CONFIG)).toBeNull();
  accum.pushLatency("latency", 21, false, 200);
  const result = accum.partialLatencyResult(DEFAULT_CONFIG);
  expect(result?.method).toBe("full-average");
  expect(result?.reportedMs).toBeCloseTo(20.5, 6);
});
test("long latency runs bound confidence while retaining exact result evidence", () => {
  const accum = fresh();
  for (let i = 0; i < 20_000; i++)
    accum.pushLatency("latency", 10 + (i % 5), i % 10 === 0, i * 250);
  expect(accum.confidence("latency").sampleCount).toBe(16);
  const result = accum.latencyResult(DEFAULT_CONFIG)!;
  expect(result.probeTimeoutPct).toBe(10);
  expect(result.minMs).toBe(10);
  expect(result.p95Ms).toBe(14);
});
test("partial bidirectional keeps each qualifying lane independently", () => {
  const accum = fresh();
  accum.pushThroughput("bidirectional", "down", 800, 0.8);
  accum.pushThroughput("bidirectional", "up", 799, 0.799);
  const result = accum.partialBidirectionalResult();
  expect(result.down?.reportedBytesPerSec).toBeCloseTo(1_000, 6);
  expect(result.up).toBeNull();
});
test("bufferbloat is unavailable without both idle and loaded latency evidence", () => {
  const accum = fresh();
  accum.pushLatency("latency", 20, false);
  expect(accum.bufferbloatGrade()).toBeNull();
  accum.pushLatency("download", 40, false);
  expect(accum.bufferbloatGrade()?.increaseMs).toBe(20);
});
