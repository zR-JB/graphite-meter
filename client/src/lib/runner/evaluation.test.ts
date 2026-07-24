import { test, expect } from "bun:test";
import { RunAccumulator } from "./evaluation";
import type { AdaptiveDurationConfig } from "./contract";

// Regression coverage: once early stopping arms for a phase, the headline must
// be the mean of the ENTIRE early-stopping phase (arm → end) when stability
// holds throughout, else the mean of the ENTIRE measurement phase (start → end).
// Never the trailing contiguous stable run, a misleading subset of both.

const adaptive: AdaptiveDurationConfig = {
  enabled: true,
  minCoverageRatio: 0,
  stabilityThreshold: 0.9,
  maxPhaseReductionRatio: 1,
  minLatencySamples: 0,
  minTransferSamples: 0,
  glideMs: 100,
};

/** Push one download sample and drive the same trackStableRun call site
 *  core.ts makes every tick, mirroring the real ingest → confidence →
 *  stable-run-latch sequence. */
function push(accum: RunAccumulator, value: number): void {
  accum.pushThroughput("download", "down", value, value, 1);
  const conf = accum.confidence("download");
  accum.trackStableRun("download", conf.score, adaptive);
}

function mean(values: number[]): number {
  return values.reduce((s, v) => s + v, 0) / values.length;
}

test("early stop reports the entire uninterrupted early-completion window", () => {
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
  // samples) is pure plateau, latching a stable run well ahead of the arm.
  for (let i = 0; i < 90; i++) {
    samples.push(1000);
    push(accum, 1000);
  }
  // Early stop arms here: coverage/threshold gates satisfied mid-plateau, the
  // same as core.ts's #maybeArmGlide calling this the instant shouldExitPhase
  // first returns true.
  accum.noteEarlyStop("download");
  const armIndex = samples.length;
  // Stays perfectly stable for the rest of the early-stopping phase.
  for (let i = 0; i < 10; i++) {
    samples.push(1000);
    push(accum, 1000);
  }

  const result = accum.throughputResult("download");

  expect(armIndex).toBeGreaterThan(0);
  expect(result.method).toBe("stable-window");
  expect(result.meanBytesPerSec).toBeCloseTo(
    mean(samples.slice(armIndex - 1)),
    6,
  );
  expect(result.fullAverageBytesPerSec).toBeCloseTo(mean(samples), 6);
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
  // ...and fully recovers on a NEW stable run by the phase end. The regression
  // case: a naive "still stable at finish" check reports only that run's 3000.
  for (let i = 0; i < 100; i++) {
    samples.push(3000);
    push(accum, 3000);
  }

  const result = accum.throughputResult("download");

  expect(result.method).toBe("full-average");
  expect(result.meanBytesPerSec).toBeCloseTo(mean(samples), 6);
  expect(result.meanBytesPerSec).toBeCloseTo(result.fullAverageBytesPerSec, 6);
  // Must NOT be the trailing-run-only figure.
  expect(result.meanBytesPerSec).not.toBeCloseTo(3000, 0);
});

test("a trailing stable run does not hide the earlier ramp", () => {
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
  // Early stop never arms; the ramp must still remain in the result.

  const result = accum.throughputResult("download");

  expect(result.method).toBe("full-average");
  expect(result.meanBytesPerSec).toBeCloseTo(mean(samples), 6);
});

// Bidirectional coverage: the phase carries two concurrent lanes (down + up)
// reduced independently, but shares a single combined-rate stability signal.
// These tests are about lane bookkeeping, not adaptive phase duration.

test("transfer headline weights samples by represented time", () => {
  const accum = new RunAccumulator();
  accum.reset();
  accum.pushThroughput("upload", "up", 100, 10, 0.1);
  accum.pushThroughput("upload", "up", 10, 10, 1);

  const result = accum.throughputResult("upload");
  expect(result.fullAverageBytesPerSec).toBeCloseTo(20 / 1.1, 6);
  expect(result.fullAverageBytesPerSec).not.toBeCloseTo(55, 6);
});

test("bidirectional: down and up lanes reduce independently", () => {
  const accum = new RunAccumulator();
  accum.reset();

  for (let i = 0; i < 30; i++) {
    accum.pushThroughput("bidirectional", "down", 500, 500, 1);
    accum.pushThroughput("bidirectional", "up", 300, 300, 1, true);
  }

  const result = accum.bidirectionalResult();
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(500, 6);
  expect(result.up.fullAverageBytesPerSec).toBeCloseTo(300, 6);
  expect(result.down.serverAuthoritative).toBeUndefined();
  expect(result.up.serverAuthoritative).toBe(true);
  expect(result.down.totalBytes).toBeCloseTo(500 * 30, 6);
  expect(result.up.totalBytes).toBeCloseTo(300 * 30, 6);
});

test("bidirectional: interleaved arrival order doesn't cross-contaminate the lanes", () => {
  const accum = new RunAccumulator();
  accum.reset();

  const downs = [400, 420, 440, 460];
  const ups = [100, 120, 140, 160];
  for (let i = 0; i < downs.length; i++) {
    accum.pushThroughput("bidirectional", "up", ups[i], ups[i], 1);
    accum.pushThroughput("bidirectional", "down", downs[i], downs[i], 1);
  }

  const result = accum.bidirectionalResult();
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(mean(downs), 6);
  expect(result.up.fullAverageBytesPerSec).toBeCloseTo(mean(ups), 6);
});

test("bidirectional: one lane still empty (staggered start) reports the other correctly", () => {
  const accum = new RunAccumulator();
  accum.reset();

  // The download lane reports while upload has sent nothing, mirroring the real
  // backend's staggered lane spawn.
  for (let i = 0; i < 10; i++) {
    accum.pushThroughput("bidirectional", "down", 700, 700, 1);
  }

  const result = accum.bidirectionalResult();
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(700, 6);
  expect(result.up.fullAverageBytesPerSec).toBe(0);
  expect(result.up.totalBytes).toBe(0);
});

test("bidirectional: shared stability degrades when either lane alone turns erratic", () => {
  const stable = new RunAccumulator();
  stable.reset();
  for (let i = 0; i < 40; i++) {
    stable.pushThroughput("bidirectional", "down", 500, 500, 1);
    stable.pushThroughput("bidirectional", "up", 300, 300, 1);
  }
  const stableScore = stable.confidence("bidirectional").score;

  const erratic = new RunAccumulator();
  erratic.reset();
  for (let i = 0; i < 40; i++) {
    const d = i % 2 === 0 ? 100 : 900; // down swings wildly...
    erratic.pushThroughput("bidirectional", "down", d, d, 1);
    erratic.pushThroughput("bidirectional", "up", 300, 300, 1); // ...up alone stays steady
  }
  const erraticScore = erratic.confidence("bidirectional").score;

  // The single stability window is fed by BOTH lanes' pushes: an erratic down
  // lane alone still drags the shared score down while up never wavers.
  expect(stableScore).toBeGreaterThan(erraticScore);
});
