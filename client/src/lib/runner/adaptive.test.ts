import { test, expect } from "bun:test";
import {
  standardDeviation,
  transferConfidence,
  latencyConfidence,
  shouldExitPhase,
  type ExitDecisionInput,
} from "./adaptive";
import type { AdaptiveDurationConfig } from "./contract";

const timed = (values: (number | null)[], cadenceMs = 100) =>
  values.map((rttMs, index) => ({ tMs: index * cadenceMs, rttMs }));

// ---------- standardDeviation ----------

test("standardDeviation: empty array is 0", () => {
  expect(standardDeviation([])).toBe(0);
});

test("standardDeviation: single-element array is 0", () => {
  expect(standardDeviation([42])).toBe(0);
});

test("standardDeviation: uniform array is 0", () => {
  expect(standardDeviation([5, 5, 5, 5, 5])).toBe(0);
});

test("standardDeviation: known-variance array matches hand-computed population stdev", () => {
  // mean = 3; squared deviations = 4,1,0,1,4 -> variance = 10/5 = 2
  expect(standardDeviation([1, 2, 3, 4, 5])).toBeCloseTo(Math.sqrt(2), 10);
});

// ---------- transferConfidence ----------

test("transferConfidence: fewer than 2 samples signals no confidence", () => {
  expect(transferConfidence([]).score).toBe(0);
  expect(transferConfidence([1000]).score).toBe(0);
});

test("transferConfidence: a flat plateau is high confidence", () => {
  const values = Array(60).fill(1000);
  const conf = transferConfidence(values);
  expect(conf.score).toBeCloseTo(1, 10);
  expect(conf.varianceRatio).toBeCloseTo(0, 10);
  expect(conf.slopeRatio).toBeCloseTo(0, 10);
});

test("transferConfidence: a noisy, drifting sequence is low confidence", () => {
  const values: number[] = [];
  for (let i = 0; i < 60; i++) {
    values.push(i % 2 === 0 ? 100 : 4000);
  }
  const conf = transferConfidence(values);
  expect(conf.score).toBe(0);
});

// ---------- latencyConfidence ----------

test("latencyConfidence: fewer than 2 samples signals no confidence", () => {
  expect(latencyConfidence([]).score).toBe(0);
  expect(latencyConfidence(timed([20])).score).toBe(0);
});

test("latencyConfidence: steady RTT with no loss is high confidence", () => {
  const values = Array(60).fill(20);
  const conf = latencyConfidence(timed(values));
  expect(conf.score).toBeCloseTo(1, 10);
  expect(conf.lossRatio).toBe(0);
});

test("latencyConfidence: jittery RTT is low confidence", () => {
  const values: number[] = [];
  for (let i = 0; i < 60; i++) {
    values.push(i % 2 === 0 ? 5 : 500);
  }
  const conf = latencyConfidence(timed(values));
  expect(conf.score).toBe(0);
});

test("latencyConfidence: steady RTT but heavy loss is still low confidence", () => {
  const values = [...Array(20).fill(null), ...Array(20).fill(20)];
  const conf = latencyConfidence(timed(values));
  expect(conf.jitterRatio).toBeCloseTo(0, 10);
  expect(conf.lossRatio).toBeCloseTo(0.5, 10);
  expect(conf.score).toBeLessThan(0.6);
});

test("latencyConfidence: ordinary low-latency jitter reaches high confidence", () => {
  const values = Array.from({ length: 48 }, (_, i) => 5 + (i % 3) - 1);
  const conf = latencyConfidence(timed(values));
  expect(conf.jitterRatio).toBeCloseTo(0.05, 10);
  expect(conf.score).toBeGreaterThan(0.86);
});

test("latencyConfidence: recovered loss ages out with the RTT window", () => {
  const values = [...Array(12).fill(null), ...Array(60).fill(20)];
  const conf = latencyConfidence(timed(values));
  expect(conf.lossRatio).toBe(0);
  expect(conf.score).toBe(1);
});

test("latencyConfidence: slow cadence retains the default early-exit floor", () => {
  const confidence = latencyConfidence(timed(Array(8).fill(20), 600));
  expect(confidence.sampleCount).toBe(8);
  expect(
    shouldExitPhase({
      kind: "latency",
      elapsedMs: 4_200,
      durationMs: 6_000,
      confidence,
      cfg: cfg({ minLatencySamples: 8 }),
    }),
  ).toBe(true);
});

// ---------- shouldExitPhase ----------

function cfg(
  overrides: Partial<AdaptiveDurationConfig> = {},
): AdaptiveDurationConfig {
  return {
    enabled: true,
    minCoverageRatio: 0.5,
    stabilityThreshold: 0.9,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 5,
    minTransferSamples: 20,
    confirmationMs: 100,
    ...overrides,
  };
}

function input(overrides: Partial<ExitDecisionInput> = {}): ExitDecisionInput {
  return {
    kind: "transfer",
    elapsedMs: 600,
    durationMs: 1000,
    confidence: {
      score: 0.95,
      varianceRatio: 0.01,
      slopeRatio: 0.01,
      sampleCount: 30,
    },
    cfg: cfg(),
    ...overrides,
  };
}

test("shouldExitPhase: true once coverage, stability, and sample floor all hold", () => {
  expect(shouldExitPhase(input())).toBe(true);
});

test("shouldExitPhase: false when adaptive is disabled", () => {
  expect(shouldExitPhase(input({ cfg: cfg({ enabled: false }) }))).toBe(false);
});

test("shouldExitPhase: false for a degenerate (zero-duration) phase", () => {
  expect(shouldExitPhase(input({ durationMs: 0 }))).toBe(false);
});

test("shouldExitPhase: false below the coverage floor", () => {
  expect(shouldExitPhase(input({ elapsedMs: 400 }))).toBe(false);
});

test("shouldExitPhase: false below the stability threshold", () => {
  expect(
    shouldExitPhase(
      input({
        confidence: {
          score: 0.5,
          varianceRatio: 0.3,
          slopeRatio: 0.1,
          sampleCount: 30,
        },
      }),
    ),
  ).toBe(false);
});

test("shouldExitPhase: false below the sample-count floor", () => {
  expect(
    shouldExitPhase(
      input({
        confidence: {
          score: 0.95,
          varianceRatio: 0.01,
          slopeRatio: 0.01,
          sampleCount: 5,
        },
      }),
    ),
  ).toBe(false);
});

test("shouldExitPhase: coverage requirement is never below (1 - maxPhaseReductionRatio)", () => {
  // minCoverageRatio alone would allow this at 65%, but maxPhaseReductionRatio
  // caps the cut to 30%, requiring 70% coverage.
  const strictCfg = cfg({ minCoverageRatio: 0, maxPhaseReductionRatio: 0.3 });
  expect(
    shouldExitPhase(
      input({ cfg: strictCfg, elapsedMs: 650, durationMs: 1000 }),
    ),
  ).toBe(false);
  expect(
    shouldExitPhase(
      input({ cfg: strictCfg, elapsedMs: 750, durationMs: 1000 }),
    ),
  ).toBe(true);
});

test("shouldExitPhase: the sample-count floor is picked per phase kind", () => {
  const sharedConfidence = {
    score: 0.95,
    jitterRatio: 0.01,
    lossRatio: 0,
    sampleCount: 10,
  };
  // 10 samples clears the latency floor (5) but not the transfer floor (20).
  expect(
    shouldExitPhase(input({ kind: "latency", confidence: sharedConfidence })),
  ).toBe(true);
  expect(
    shouldExitPhase(input({ kind: "transfer", confidence: sharedConfidence })),
  ).toBe(false);
});
