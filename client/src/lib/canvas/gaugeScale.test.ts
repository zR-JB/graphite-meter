import { expect, test } from "bun:test";
import {
  fmtGaugeTick,
  gaugeScaleForPeak,
  throughputGaugeFraction,
  throughputValueAtFraction,
  THROUGHPUT_VALUE_KNOTS,
} from "./gaugeScale";
import { rateValueAt } from "../format";

test("piecewise throughput transfer is monotonic, bounded, and invertible", () => {
  let previous = 0;
  for (const value of [
    ...THROUGHPUT_VALUE_KNOTS,
    0.125,
    0.375,
    0.625,
    0.875,
  ].sort((a, b) => a - b)) {
    const fraction = throughputGaugeFraction(value * 1_000, 1_000);
    expect(fraction).toBeGreaterThanOrEqual(previous);
    expect(fraction).toBeGreaterThanOrEqual(0);
    expect(fraction).toBeLessThanOrEqual(1);
    expect(throughputValueAtFraction(fraction, 1_000)).toBeCloseTo(
      value * 1_000,
      8,
    );
    previous = fraction;
  }
});

test("one-gigabit gauge labels use Mbit/s and exact five-label table", () => {
  const scale = gaugeScaleForPeak(125_000_000, {
    minimumBitsPerSec: 1_000_000_000,
  });
  expect(scale).toBe(125_000_000);
  expect(
    [0, 0.25, 0.5, 0.75, 1].map((fraction) =>
      fmtGaugeTick(
        rateValueAt(
          throughputValueAtFraction(fraction, scale),
          "base10",
          "bits",
          2,
        ),
      ),
    ),
  ).toEqual(["0", "10", "100", "500", "1000"]);
});

test("automatic gauge scale stays at one gigabit until it exceeds it", () => {
  const automatic = { minimumBitsPerSec: 1_000_000_000 };
  expect(gaugeScaleForPeak(1, automatic)).toBe(125_000_000);
  expect(gaugeScaleForPeak(125_000_000, automatic)).toBe(125_000_000);
  expect(gaugeScaleForPeak(125_000_001, automatic)).toBe(1_250_000_000);
});

test("a sub-mega automatic scale waives the one-gigabit floor", () => {
  expect(gaugeScaleForPeak(12_501)).toBe(125_000);
});

test("explicit chart ceilings stay exact while gauge uses the next decade", () => {
  expect(gaugeScaleForPeak(117_125_000)).toBe(125_000_000);
  expect(gaugeScaleForPeak(12_500_000)).toBe(12_500_000);
  expect(fmtGaugeTick(1_234.5)).toBe("1235");
  expect(fmtGaugeTick(0)).toBe("0");
});

test("SI, byte, and IEC gauge labels remain truthful and ungrouped", () => {
  for (const [base, kind] of [
    ["base10", "bits"],
    ["base10", "bytes"],
    ["base2", "bytes"],
  ] as const) {
    const scale = gaugeScaleForPeak(125_000_000, {
      minimumBitsPerSec: 1_000_000_000,
    });
    const values = [0, 0.25, 0.5, 0.75, 1].map((fraction) =>
      fmtGaugeTick(
        rateValueAt(throughputValueAtFraction(fraction, scale), base, kind, 2),
      ),
    );
    expect(values.every((value) => !value.includes(","))).toBe(true);
    expect(values[0]).toBe("0");
  }
});
