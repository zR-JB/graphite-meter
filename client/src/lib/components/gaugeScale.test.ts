import { expect, test } from "bun:test";
import {
  fmtGaugeTick,
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

test("SI, byte, and IEC gauge labels remain truthful and ungrouped", () => {
  for (const [base, kind] of [
    ["base10", "bits"],
    ["base10", "bytes"],
    ["base2", "bytes"],
  ] as const) {
    const values = [0, 0.25, 0.5, 0.75, 1].map((fraction) =>
      fmtGaugeTick(
        rateValueAt(
          throughputValueAtFraction(fraction, 125_000_000),
          base,
          kind,
          2,
        ),
      ),
    );
    expect(values.every((value) => !value.includes(","))).toBe(true);
    expect(values[0]).toBe("0");
  }
});
