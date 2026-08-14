import { test, expect } from "bun:test";
import {
  MIN_PARTIAL_LATENCY_OUTCOMES,
  MIN_PARTIAL_TRANSFER_EVIDENCE_MS,
  RunAccumulator,
} from "./evaluation";
import type { AdaptiveDurationConfig } from "./contract";
import { DEFAULT_CONFIG } from "../state/defaults";

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
      rate,
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
  const splitCallbacks = Array(rates.length * 5).fill(50);
  const whole = descriptiveStability(rates, wholeBuckets);

  const split = new RunAccumulator();
  split.reset();
  for (const rate of rates) {
    for (let i = 0; i < 5; i++)
      split.pushThroughput("download", "down", rate, rate * 0.05, 0.05);
  }
  expect(whole).toBeCloseTo(95, 8);
  expect(split.throughputResult("download", false).stabilityPct).toBeCloseTo(
    whole,
    8,
  );
  expect(splitCallbacks.length).toBeGreaterThan(wholeBuckets.length);
});

test.each([
  { cv: 5, rates: [950, 1050, 950, 1050, 1050, 950, 1050, 950], band: "high" },
  {
    cv: 15,
    rates: [850, 1150, 850, 1150, 1150, 850, 1150, 850],
    band: "medium",
  },
  {
    cv: 30,
    rates: [700, 1300, 700, 1300, 1300, 700, 1300, 700],
    band: "low",
  },
])(
  "descriptive stability reports approximately $cv% CV as the $band band",
  ({ rates, cv, band }) => {
    const stabilityPct = descriptiveStability(
      rates,
      Array(rates.length).fill(250),
    );
    expect(stabilityPct).toBeCloseTo(100 - cv, 5);
    const score = stabilityPct / 100;
    const displayBand =
      score >= 0.9 ? "high" : score >= 0.75 ? "medium" : "low";
    expect(displayBand).toBe(band);
  },
);

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

test("stable evidence does not select a window unless early completion shortened the stage", () => {
  const accum = new RunAccumulator();
  accum.reset();
  for (let i = 0; i < 4; i++) {
    accum.pushThroughput("download", "down", 100, 100, 1);
    accum.trackStableRun("download", 0, adaptive);
  }
  for (let i = 0; i < 4; i++) {
    accum.pushThroughput("download", "down", 1000, 1000, 1);
    accum.trackStableRun("download", 1, adaptive);
  }

  const result = accum.throughputResult("download", false);

  expect(result.method).toBe("full-average");
  expect(result.reportedBytesPerSec).toBeCloseTo(550, 6);
  expect(result.fullAverageBytesPerSec).toBeCloseTo(550, 6);
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
  accum.trackStableRun("download", 0, adaptive);
  accum.pushThroughput("download", "down", 100, 100, 1);
  accum.trackStableRun("download", 1, adaptive);

  const result = accum.throughputResult("download", true);
  expect(result.method).toBe("stable-window");
  // The final observation opens stability and remains exact weighted evidence
  // for the reported trailing plateau.
  expect(result.meanBytesPerSec).toBeCloseTo(100, 6);
});

test("stability first established on the final one-way observation remains reducible", () => {
  const accum = new RunAccumulator();
  accum.reset();

  accum.pushThroughput("upload", "up", 1000, 1000, 1);
  accum.trackStableRun("upload", 0, adaptive);
  accum.pushThroughput("upload", "up", 250, 500, 2, true);
  accum.trackStableRun("upload", 1, adaptive);

  const result = accum.throughputResult("upload", true);
  expect(result.method).toBe("stable-window");
  expect(result.meanBytesPerSec).toBeCloseTo(250, 6);
  expect(result.fullAverageBytesPerSec).toBeCloseTo(500, 6);
  expect(result.serverAuthoritative).toBe(true);
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
  pushBidi("down", 500, 0);
  // Re-entry happens on upload after both lanes have supplied fresh plateau
  // evidence. Each lane's opening observation belongs to the stable result.
  pushBidi("up", 300, 1);
  pushBidi("down", 500, 1);
  pushBidi("up", 300, 1);

  const result = accum.bidirectionalResult(true);
  expect(result.down.method).toBe("stable-window");
  expect(result.up.method).toBe("stable-window");
  expect(result.down.meanBytesPerSec).toBeCloseTo(500, 6);
  expect(result.up.meanBytesPerSec).toBeCloseTo(300, 6);
});

test("bidirectional stable evidence also requires actual early completion", () => {
  const accum = new RunAccumulator();
  accum.reset();
  const pushBidi = (dir: "down" | "up", value: number, score: number): void => {
    accum.pushThroughput("bidirectional", dir, value, value, 1);
    accum.trackStableRun("bidirectional", score, adaptive);
  };

  for (let i = 0; i < 4; i++) {
    pushBidi("down", 100, 0);
    pushBidi("up", 50, 0);
  }
  for (let i = 0; i < 4; i++) {
    pushBidi("down", 1000, 1);
    pushBidi("up", 500, 1);
  }

  const result = accum.bidirectionalResult(false);

  expect(result.down.method).toBe("full-average");
  expect(result.up.method).toBe("full-average");
  expect(result.down.reportedBytesPerSec).toBeCloseTo(550, 6);
  expect(result.up.reportedBytesPerSec).toBeCloseTo(275, 6);
});

test("final bidirectional observation preserves the opening evidence in both lanes", () => {
  const accum = new RunAccumulator();
  accum.reset();
  const pushBidi = (dir: "down" | "up", value: number, score: number): void => {
    accum.pushThroughput("bidirectional", dir, value, value, 1);
    accum.trackStableRun("bidirectional", score, adaptive);
  };

  pushBidi("down", 100, 0);
  pushBidi("up", 50, 0);
  pushBidi("down", 500, 0);
  pushBidi("up", 300, 1);

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

test("partial transfer keeps whole exact evidence only after its named floor", () => {
  const accum = new RunAccumulator();
  accum.reset();
  accum.pushThroughput(
    "download",
    "down",
    1_000,
    700,
    (MIN_PARTIAL_TRANSFER_EVIDENCE_MS - 100) / 1_000,
  );
  expect(accum.partialThroughputResult("download")).toBeNull();

  accum.pushThroughput("download", "down", 1_000, 100, 0.1);
  const result = accum.partialThroughputResult("download");
  expect(result?.method).toBe("full-average");
  expect(result?.totalBytes).toBe(800);
  expect(result?.reportedBytesPerSec).toBeCloseTo(1_000, 6);
});

test("a recovery gap reaches only the final byte/time reduction", () => {
  const accum = new RunAccumulator();
  accum.pushThroughput("upload", "up", 1_000, 1_000, 1, true);
  const controlSamples = accum.confidence("upload").sampleCount;
  accum.recordRecoveryGap("upload", "up", 1);

  const result = accum.partialThroughputResult("upload");
  expect(result?.totalBytes).toBe(1_000);
  expect(result?.fullAverageBytesPerSec).toBe(500);
  // The gap did not add a second control observation, so a later source frame
  // still has the same fixed-bucket result as it would without a handoff.
  expect(accum.confidence("upload").sampleCount).toBe(controlSamples);
});

test("replacement checkpoint bytes enter only the exact final reduction", () => {
  const accum = new RunAccumulator();
  accum.recordRecoveryGap("upload", "up", 1);
  accum.recordRecoveryBytes("upload", "up", 1_000);

  const result = accum.partialThroughputResult("upload");
  expect(result).not.toBeNull();
  expect(result?.totalBytes).toBe(1_000);
  expect(result?.fullAverageBytesPerSec).toBe(1_000);
  expect(result?.serverAuthoritative).toBe(true);
});

test("partial latency needs named outcome and success evidence floors", () => {
  const accum = new RunAccumulator();
  accum.reset();
  for (let i = 0; i < MIN_PARTIAL_LATENCY_OUTCOMES - 1; i++)
    accum.pushLatency(20, false, i > 0, i * 100);
  expect(accum.partialLatencyResult(DEFAULT_CONFIG, 0)).toBeNull();

  accum.pushLatency(21, false, false, 200);
  const result = accum.partialLatencyResult(DEFAULT_CONFIG, 0);
  expect(result?.method).toBe("full-average");
  expect(result?.reportedMs).toBeCloseTo(20.5, 6);
});

test("partial bidirectional keeps each qualifying lane independently", () => {
  const accum = new RunAccumulator();
  accum.reset();
  accum.pushThroughput("bidirectional", "down", 1_000, 800, 0.8);
  accum.pushThroughput("bidirectional", "up", 1_000, 799, 0.799);
  const result = accum.partialBidirectionalResult();
  expect(result.down?.reportedBytesPerSec).toBeCloseTo(1_000, 6);
  expect(result.up).toBeNull();
});

test("bufferbloat is unavailable without both idle and loaded latency evidence", () => {
  const accum = new RunAccumulator();
  accum.reset();
  accum.pushLatency(20, false, false);
  expect(accum.bufferbloatGrade()).toBeNull();
  accum.pushLatency(40, true, false);
  expect(accum.bufferbloatGrade()?.increaseMs).toBe(20);
});
