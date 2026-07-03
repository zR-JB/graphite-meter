import { test, expect } from "bun:test";
import { RunAccumulator } from "./evaluation";
import type { AdaptiveDurationConfig, RunnerConfig } from "./contract";

// Regression coverage: once early stopping actually arms for a phase,
// the reported headline must be either
//   (a) the average of the ENTIRE early-stopping phase (arm point → end) when
//       stability holds the whole way through, or
//   (b) the average of the ENTIRE measurement phase (start → end) when
//       stability breaks at any point after arming —
// never just the trailing contiguous stable run, which can be a strict
// (and misleading) subset of both.

const adaptive: AdaptiveDurationConfig = {
  enabled: true,
  minCoverageRatio: 0,
  stabilityThreshold: 0.9,
  maxPhaseReductionRatio: 1,
  minLatencySamples: 0,
  minTransferSamples: 0,
  glideMs: 100,
};
const cfg = { adaptive } as unknown as RunnerConfig;

/** Push one download sample and drive the same trackStableRun call site
 *  core.ts makes every tick, mirroring the real ingest → confidence →
 *  stable-run-latch sequence. */
function push(accum: RunAccumulator, value: number): void {
  accum.pushThroughput("download", "down", value, value);
  const conf = accum.confidence("download");
  accum.trackStableRun("download", conf.score, adaptive);
}

function mean(values: number[]): number {
  return values.reduce((s, v) => s + v, 0) / values.length;
}

test("early stop stable throughout → averages the entire early-stopping phase", () => {
  const accum = new RunAccumulator();
  accum.reset();

  const samples: number[] = [];
  // Noisy ramp: keeps the confidence score well under threshold, so no
  // stable run latches yet.
  for (let i = 0; i < 60; i++) {
    const v = 100 + i * (800 / 59);
    samples.push(v);
    push(accum, v);
  }
  // Flat plateau: score converges to 1 once the confidence window (last 48
  // samples) is pure plateau, latching a stable run well before we arm.
  for (let i = 0; i < 90; i++) {
    samples.push(1000);
    push(accum, 1000);
  }
  // Early stop arms here — coverage/threshold gates satisfied mid-plateau,
  // same as core.ts's #maybeArmGlide calling this the instant shouldExitPhase
  // first returns true.
  accum.noteEarlyStop("download");
  const armIndex = samples.length;
  // Stays perfectly stable for the rest of the early-stopping phase.
  for (let i = 0; i < 10; i++) {
    samples.push(1000);
    push(accum, 1000);
  }

  const result = accum.throughputResult("download", cfg);

  expect(result.method).toBe("stable-window");
  expect(result.meanBytesPerSec).toBeCloseTo(mean(samples.slice(armIndex)), 6);
  expect(result.meanBytesPerSec).toBeCloseTo(1000, 6);
  // Sanity: the early-stopping-phase average must differ from the full
  // measurement-phase average (the ramp drags the latter down) — otherwise
  // this test couldn't distinguish the fixed behavior from the old bug.
  expect(result.meanBytesPerSec).not.toBeCloseTo(
    result.fullAverageBytesPerSec,
    0,
  );
});

test("early stop destabilizes after arming → averages the entire measurement phase", () => {
  const accum = new RunAccumulator();
  accum.reset();

  const samples: number[] = [];
  for (let i = 0; i < 60; i++) {
    const v = 100 + i * (800 / 59);
    samples.push(v);
    push(accum, v);
  }
  for (let i = 0; i < 90; i++) {
    samples.push(1000);
    push(accum, 1000);
  }
  accum.noteEarlyStop("download");
  // Stability breaks during the early-stopping period...
  for (let i = 0; i < 20; i++) {
    const v = i % 2 === 0 ? 100 : 2000;
    samples.push(v);
    push(accum, v);
  }
  // ...and even fully recovers on a NEW stable run before the phase ends —
  // this is the exact regression case: a naive "still stable at finish"
  // check would wrongly report just this trailing run's mean (3000) instead
  // of the whole phase's.
  for (let i = 0; i < 100; i++) {
    samples.push(3000);
    push(accum, 3000);
  }

  const result = accum.throughputResult("download", cfg);

  expect(result.method).toBe("full-average");
  expect(result.meanBytesPerSec).toBeCloseTo(mean(samples), 6);
  expect(result.meanBytesPerSec).toBeCloseTo(result.fullAverageBytesPerSec, 6);
  // Must NOT be the trailing-run-only figure the old bug would have reported.
  expect(result.meanBytesPerSec).not.toBeCloseTo(3000, 0);
});

test("no early stop, still stable at finish → falls back to the trailing stable-run window", () => {
  const accum = new RunAccumulator();
  accum.reset();

  const samples: number[] = [];
  for (let i = 0; i < 60; i++) {
    const v = 100 + i * (800 / 59);
    samples.push(v);
    push(accum, v);
  }
  for (let i = 0; i < 60; i++) {
    samples.push(1000);
    push(accum, 1000);
  }
  // Early stop never armed for this phase (e.g. it ran to its natural end) —
  // pre-existing behavior is preserved: trailing stable window, not full avg.

  const result = accum.throughputResult("download", cfg);

  expect(result.method).toBe("stable-window");
  expect(result.meanBytesPerSec).toBeCloseTo(1000, 6);
  expect(result.meanBytesPerSec).not.toBeCloseTo(
    result.fullAverageBytesPerSec,
    0,
  );
});

// Bidirectional coverage: the phase carries two concurrent lanes (down + up)
// reduced independently, but shares a single combined-rate stability signal.
// Adaptive is off here — these tests are about lane bookkeeping, not the
// early-stop window logic already covered above.
const noAdaptive: AdaptiveDurationConfig = {
  enabled: false,
  minCoverageRatio: 0,
  stabilityThreshold: 0.9,
  maxPhaseReductionRatio: 1,
  minLatencySamples: 0,
  minTransferSamples: 0,
  glideMs: 0,
};
const bidiCfg = { adaptive: noAdaptive } as unknown as RunnerConfig;

test("bidirectional: down and up lanes reduce independently", () => {
  const accum = new RunAccumulator();
  accum.reset();

  for (let i = 0; i < 30; i++) {
    accum.pushThroughput("bidirectional", "down", 500, 500);
    accum.pushThroughput("bidirectional", "up", 300, 300);
  }

  const result = accum.bidirectionalResult(bidiCfg);
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(500, 6);
  expect(result.up.fullAverageBytesPerSec).toBeCloseTo(300, 6);
  expect(result.down.totalBytes).toBeCloseTo(500 * 30, 6);
  expect(result.up.totalBytes).toBeCloseTo(300 * 30, 6);
});

test("bidirectional: interleaved arrival order doesn't cross-contaminate the lanes", () => {
  const accum = new RunAccumulator();
  accum.reset();

  const downs = [400, 420, 440, 460];
  const ups = [100, 120, 140, 160];
  for (let i = 0; i < downs.length; i++) {
    accum.pushThroughput("bidirectional", "up", ups[i], ups[i]);
    accum.pushThroughput("bidirectional", "down", downs[i], downs[i]);
  }

  const result = accum.bidirectionalResult(bidiCfg);
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(mean(downs), 6);
  expect(result.up.fullAverageBytesPerSec).toBeCloseTo(mean(ups), 6);
});

test("bidirectional: one lane still empty (staggered start) reports the other correctly", () => {
  const accum = new RunAccumulator();
  accum.reset();

  // Download lane has started reporting; upload hasn't sent a sample yet —
  // mirrors the real backend's staggered lane spawn.
  for (let i = 0; i < 10; i++) {
    accum.pushThroughput("bidirectional", "down", 700, 700);
  }

  const result = accum.bidirectionalResult(bidiCfg);
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(700, 6);
  expect(result.up.fullAverageBytesPerSec).toBe(0);
  expect(result.up.totalBytes).toBe(0);
});

test("bidirectional: shared stability degrades when either lane alone turns erratic", () => {
  const stable = new RunAccumulator();
  stable.reset();
  for (let i = 0; i < 40; i++) {
    stable.pushThroughput("bidirectional", "down", 500, 500);
    stable.pushThroughput("bidirectional", "up", 300, 300);
  }
  const stableScore = stable.confidence("bidirectional").score;

  const erratic = new RunAccumulator();
  erratic.reset();
  for (let i = 0; i < 40; i++) {
    const d = i % 2 === 0 ? 100 : 900; // down swings wildly...
    erratic.pushThroughput("bidirectional", "down", d, d);
    erratic.pushThroughput("bidirectional", "up", 300, 300); // ...up alone stays steady
  }
  const erraticScore = erratic.confidence("bidirectional").score;

  // Proves the single stability window is fed by BOTH lanes' pushes (down+up
  // combined), not just one — an erratic down lane alone must still drag the
  // shared score down even though up never wavers.
  expect(stableScore).toBeGreaterThan(erraticScore);
});
