import { test, expect } from "bun:test";
import { clamp01, sweepTarget, angleForFraction } from "./gaugeSweep";
import type { SweepTargetInput } from "./gaugeSweep";

const base: SweepTargetInput = {
  phase: "idle",
  valueBytesPerSec: 0,
  scaleBytesPerSec: 1000,
  latencyScaleMs: 100,
  rtt: 0,
  frozenFraction: 0,
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
    ).toBeCloseTo(0.5, 10);
  }
});

test("sweepTarget: transfer value is clamped at the scale ceiling and floor", () => {
  expect(
    sweepTarget({
      ...base,
      phase: "download",
      valueBytesPerSec: 5000,
      scaleBytesPerSec: 1000,
    }),
  ).toBe(1);
  expect(
    sweepTarget({
      ...base,
      phase: "download",
      valueBytesPerSec: -100,
      scaleBytesPerSec: 1000,
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
  ).toBe(0.5);
});

test("sweepTarget: warmup holds a fixed indeterminate position", () => {
  expect(sweepTarget({ ...base, phase: "warmup" })).toBe(0.3);
});

test("sweepTarget: connecting holds the same indeterminate position", () => {
  expect(sweepTarget({ ...base, phase: "connecting" })).toBe(0.3);
});

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

test("sweepTarget: idle holds a fixed indeterminate position", () => {
  expect(sweepTarget({ ...base, phase: "idle" })).toBe(0.1);
});

test("sweepTarget: complete holds the last live position", () => {
  expect(
    sweepTarget({
      ...base,
      phase: "complete",
      frozenFraction: 0.2,
    }),
  ).toBe(0.2);
});

test("sweepTarget: aborted/error hold a fixed low position", () => {
  expect(sweepTarget({ ...base, phase: "aborted" })).toBe(0.05);
  expect(sweepTarget({ ...base, phase: "error" })).toBe(0.05);
});

test("angleForFraction: 0 and 1 land on the arc's endpoints", () => {
  expect(angleForFraction(0, 1, 2)).toBe(1);
  expect(angleForFraction(1, 1, 2)).toBe(3);
});

test("angleForFraction: midpoint fraction lands halfway across the sweep", () => {
  expect(angleForFraction(0.5, 1, 2)).toBe(2);
});

test("angleForFraction: out-of-range fractions are clamped before mapping", () => {
  expect(angleForFraction(-1, 1, 2)).toBe(1);
  expect(angleForFraction(2, 1, 2)).toBe(3);
});
