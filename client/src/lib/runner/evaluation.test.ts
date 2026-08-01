import { test, expect } from "bun:test";
import { RunAccumulator } from "./evaluation";
import type { AdaptiveDurationConfig } from "./contract";

// Regression coverage for issue #84: adaptive throughput reports the final
// contiguous stable plateau. Earlier plateaus are discarded after a stability
// break; an unstable ending, or adaptive completion being off, uses the full
// measured phase instead.

const adaptive: AdaptiveDurationConfig = {
  enabled: true,
  minCoverageRatio: 0,
  stabilityThreshold: 0.9,
  maxPhaseReductionRatio: 1,
  minLatencySamples: 0,
  minTransferSamples: 0,
  confirmationMs: 100,
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

test("adaptive throughput reports the final plateau after stability recovers", () => {
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
  // Break the first plateau, then settle at a higher rate. The old reducer
  // remained anchored near the first plateau's confirmation candidate.
  for (let i = 0; i < 20; i++) {
    const v = i % 2 === 0 ? 100 : 2000;
    samples.push(v);
    push(accum, v);
  }
  for (let i = 0; i < 100; i++) {
    samples.push(3000);
    push(accum, 3000);
  }

  const result = accum.throughputResult("download", true);

  expect(result.method).toBe("stable-window");
  expect(result.meanBytesPerSec).toBeCloseTo(3000, 6);
  expect(result.fullAverageBytesPerSec).toBeCloseTo(mean(samples), 6);
  expect(result.meanBytesPerSec).not.toBeCloseTo(1000, 0);
});

test("an unstable ending falls back to the entire measurement phase", () => {
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
  for (let i = 0; i < 20; i++) {
    const v = i % 2 === 0 ? 100 : 2000;
    samples.push(v);
    push(accum, v);
  }

  const result = accum.throughputResult("download", true);

  expect(result.method).toBe("full-average");
  expect(result.meanBytesPerSec).toBeCloseTo(mean(samples), 6);
  expect(result.meanBytesPerSec).toBeCloseTo(result.fullAverageBytesPerSec, 6);
});

test("adaptive completion off always reports the entire measurement phase", () => {
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
  const result = accum.throughputResult("download", false);

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

  const result = accum.throughputResult("upload", false);
  expect(result.fullAverageBytesPerSec).toBeCloseTo(20 / 1.1, 6);
  expect(result.fullAverageBytesPerSec).not.toBeCloseTo(55, 6);
});

test("the final stable plateau is also weighted by represented time", () => {
  const accum = new RunAccumulator();
  accum.reset();

  accum.pushThroughput("download", "down", 1000, 100, 0.1);
  accum.trackStableRun("download", 1, adaptive);
  accum.pushThroughput("download", "down", 100, 100, 1);
  accum.trackStableRun("download", 1, adaptive);

  const result = accum.throughputResult("download", true);
  expect(result.method).toBe("stable-window");
  // Stability begins at an evidence boundary. The sample that establishes the
  // latch is evidence for the decision, while subsequent exact bytes/time form
  // the reported trailing plateau.
  expect(result.meanBytesPerSec).toBeCloseTo(100, 6);
});

test("bidirectional: down and up lanes reduce independently", () => {
  const accum = new RunAccumulator();
  accum.reset();

  for (let i = 0; i < 30; i++) {
    accum.pushThroughput("bidirectional", "down", 500, 500, 1);
    accum.pushThroughput("bidirectional", "up", 300, 300, 1, true);
  }

  const result = accum.bidirectionalResult(false);
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

  const result = accum.bidirectionalResult(false);
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

  const result = accum.bidirectionalResult(false);
  expect(result.down.fullAverageBytesPerSec).toBeCloseTo(700, 6);
  expect(result.up.fullAverageBytesPerSec).toBe(0);
  expect(result.up.totalBytes).toBe(0);
});

test("bidirectional final plateau aligns each interleaved lane to shared stability", () => {
  const accum = new RunAccumulator();
  accum.reset();
  const pushBidi = (dir: "down" | "up", value: number, score: number): void => {
    accum.pushThroughput("bidirectional", dir, value, value, 1);
    accum.trackStableRun("bidirectional", score, adaptive);
  };

  // First low plateau.
  pushBidi("down", 100, 1);
  pushBidi("up", 50, 1);
  pushBidi("down", 100, 1);
  pushBidi("up", 50, 1);
  // Shared stability breaks on interleaved lane reports.
  pushBidi("down", 10, 0);
  pushBidi("up", 5, 0);
  // Re-entry happens on upload. Sequence tags keep the preceding unstable
  // download sample out until download reports inside the new plateau.
  pushBidi("up", 300, 1);
  pushBidi("down", 500, 1);
  pushBidi("up", 300, 1);
  pushBidi("down", 500, 1);

  const result = accum.bidirectionalResult(true);
  expect(result.down.method).toBe("stable-window");
  expect(result.up.method).toBe("stable-window");
  expect(result.down.meanBytesPerSec).toBeCloseTo(500, 6);
  expect(result.up.meanBytesPerSec).toBeCloseTo(300, 6);
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
