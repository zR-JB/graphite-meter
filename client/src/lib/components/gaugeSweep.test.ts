import { test, expect } from "bun:test";
import { clamp01, sweepTarget, angleForFraction } from "./gaugeSweep";
import type { SweepTargetInput } from "./gaugeSweep";

const base: SweepTargetInput = {
  phase: "idle",
  valueBytesPerSec: 0,
  scaleBytesPerSec: 1000,
  throughputEvidence: true,
  latencyScaleMs: 100,
  rtt: 0,
  completedKind: "speed",
};

test("clamp01 bounds to [0,1]", () => {
  expect(clamp01(-1)).toBe(0);
  expect(clamp01(0.5)).toBe(0.5);
  expect(clamp01(2)).toBe(1);
});

test("sweepTarget: download/upload/bidirectional normalize value/scale", () => {
  for (const phase of ["download", "upload", "bidirectional"] as const) {
    expect(
      sweepTarget({
        ...base,
        phase,
        valueBytesPerSec: 500,
        scaleBytesPerSec: 1000,
      }),
    ).toBeCloseTo(0.75, 10);
  }
});

test("sweepTarget: transfer value is clamped at the scale ceiling and floor", () => {
  for (const [valueBytesPerSec, expected] of [
    [5000, 1],
    [-100, 0],
  ] as const)
    expect(sweepTarget({ ...base, phase: "download", valueBytesPerSec })).toBe(
      expected,
    );
});

test("sweepTarget: transfer remains neutral until authoritative evidence arrives", () => {
  expect(
    sweepTarget({
      ...base,
      phase: "download",
      valueBytesPerSec: 0,
      throughputEvidence: false,
    }),
  ).toBe(0.5);
  expect(
    sweepTarget({
      ...base,
      phase: "download",
      valueBytesPerSec: 0,
      throughputEvidence: true,
    }),
  ).toBe(0);
});

test("sweepTarget: a non-positive scale falls back to 1 (no divide-by-zero)", () => {
  expect(
    sweepTarget({
      ...base,
      phase: "download",
      valueBytesPerSec: 0.5,
      scaleBytesPerSec: 0,
    }),
  ).toBeCloseTo(0.75, 10);
});

for (const [name, phase, expected] of [
  ["sweepTarget: warmup holds a fixed indeterminate position", "warmup", 0.5],
  [
    "sweepTarget: connecting holds the same indeterminate position",
    "connecting",
    0.5,
  ],
  ["sweepTarget: idle holds a fixed indeterminate position", "idle", 0.5],
  ["sweepTarget: aborted holds a fixed low position", "aborted", 0.05],
  ["sweepTarget: error holds a fixed low position", "error", 0.05],
] as const) {
  test(name, () => expect(sweepTarget({ ...base, phase })).toBe(expected));
}

test("sweepTarget: latency normalizes rtt/latencyScaleMs", () => {
  expect(
    sweepTarget({ ...base, phase: "latency", rtt: 25, latencyScaleMs: 100 }),
  ).toBeCloseTo(0.25, 10);
});

test("sweepTarget: latency scale <=0 falls back to 1", () => {
  expect(
    sweepTarget({ ...base, phase: "latency", rtt: 0.5, latencyScaleMs: 0 }),
  ).toBe(0.5);
});

test("sweepTarget: completed throughput remains normalized to the current scale", () => {
  for (const [valueBytesPerSec, scaleBytesPerSec, expected] of [
    [200, 1000, 0.5833333333333333],
    [200, 400, 0.75],
  ] as const)
    expect(
      sweepTarget({
        ...base,
        phase: "complete",
        valueBytesPerSec,
        scaleBytesPerSec,
      }),
    ).toBeCloseTo(expected, 10);
});

test("sweepTarget: completed latency uses the latency scale", () => {
  expect(
    sweepTarget({
      ...base,
      phase: "complete",
      completedKind: "latency",
      rtt: 25,
    }),
  ).toBe(0.25);
});

test("angleForFraction: 0 and 1 land on the arc's endpoints", () => {
  for (const [fraction, expected] of [
    [0, 1],
    [1, 3],
  ] as const)
    expect(angleForFraction(fraction, 1, 2)).toBe(expected);
});

test("angleForFraction: midpoint fraction lands halfway across the sweep", () => {
  expect(angleForFraction(0.5, 1, 2)).toBe(2);
});

test("angleForFraction: out-of-range fractions are clamped before mapping", () => {
  expect(angleForFraction(-1, 1, 2)).toBe(1);
  expect(angleForFraction(2, 1, 2)).toBe(3);
});
