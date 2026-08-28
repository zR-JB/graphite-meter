import { test, expect } from "bun:test";
import {
  fmtBytes,
  fmtSpeed,
  rateScaleIndex,
  rateValueAt,
  niceCeil,
  quantile,
  niceStep,
  niceDomain,
  chartThroughputScale,
  DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC,
  rateUnit,
  throughputUnitIndex,
} from "./format";

test("fmtBytes: zero renders as an integer byte count", () => {
  expect(fmtBytes(0, "base10")).toBe("0 B");
});

test("fmtBytes: base10 steps on exact powers of 1000", () => {
  expect(fmtBytes(1000, "base10")).toBe("1.0 kB");
  expect(fmtBytes(1_000_000, "base10")).toBe("1.0 MB");
});

test("fmtBytes: base2 steps on exact powers of 1024", () => {
  expect(fmtBytes(1024, "base2")).toBe("1.0 KiB");
  expect(fmtBytes(1024 * 1024, "base2")).toBe("1.0 MiB");
});

test("fmtBytes: fractional values round to one decimal once scaled", () => {
  expect(fmtBytes(1500, "base10")).toBe("1.5 kB");
  expect(fmtBytes(1536, "base2")).toBe("1.5 KiB");
});

test("rateScaleIndex: stays at the base tier just below the k threshold", () => {
  expect(rateScaleIndex(999, "base10")).toBe(0);
});

test("rateScaleIndex: steps up exactly at the k threshold", () => {
  expect(rateScaleIndex(1000, "base10")).toBe(1);
  expect(rateScaleIndex(1_000_000, "base10")).toBe(2);
});

test("rateScaleIndex: base2 tiers step on powers of 1024", () => {
  expect(rateScaleIndex(1023, "base2")).toBe(0);
  expect(rateScaleIndex(1024, "base2")).toBe(1);
});

test("rateScaleIndex: headroom delays the step-up past the raw boundary", () => {
  expect(rateScaleIndex(1199, "base10", 1.2)).toBe(0);
  expect(rateScaleIndex(1200, "base10", 1.2)).toBe(1);
});

test("throughput unit authority keeps the 1.2 promotion threshold consistent", () => {
  const mbit = (gigabit: number) => (gigabit * 1_000_000_000) / 8;
  expect(
    rateUnit(
      "base10",
      "bits",
      throughputUnitIndex(mbit(0.1), "base10", "bits"),
    ),
  ).toBe("Mbit/s");
  expect(
    rateUnit(
      "base10",
      "bits",
      throughputUnitIndex(mbit(0.9), "base10", "bits"),
    ),
  ).toBe("Mbit/s");
  expect(
    rateUnit(
      "base10",
      "bits",
      throughputUnitIndex(mbit(1.19), "base10", "bits"),
    ),
  ).toBe("Mbit/s");
  expect(
    rateUnit(
      "base10",
      "bits",
      throughputUnitIndex(mbit(1.2), "base10", "bits"),
    ),
  ).toBe("Gbit/s");
  expect(
    rateUnit("base10", "bits", throughputUnitIndex(mbit(10), "base10", "bits")),
  ).toBe("Gbit/s");
});

test("throughput displays keep precision for promoted gigabit values", () => {
  expect(fmtSpeed(8.886)).toBe("8.89");
  expect(fmtSpeed(937)).toBe("937.0");
});

test("automatic unit reference starts at the 100 Mbit/s fallback", () => {
  expect(DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC * 8).toBe(100_000_000);
  expect(chartThroughputScale(0)).toBe(
    DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC,
  );
  expect(
    rateUnit(
      "base10",
      "bits",
      throughputUnitIndex(
        DEFAULT_THROUGHPUT_REFERENCE_BYTES_PER_SEC,
        "base10",
        "bits",
      ),
    ),
  ).toBe("Mbit/s");
});

test("a positive sub-mega automatic reference is not lifted to the fallback tier", () => {
  expect(
    rateUnit("base10", "bits", throughputUnitIndex(12_500, "base10", "bits")),
  ).toBe("kbit/s");
});

test("throughput unit authority handles SI bytes and IEC bytes", () => {
  expect(
    rateUnit(
      "base10",
      "bytes",
      throughputUnitIndex(1_200_000, "base10", "bytes"),
    ),
  ).toBe("MB/s");
  expect(
    rateUnit(
      "base2",
      "bytes",
      throughputUnitIndex(1_258_292, "base2", "bytes"),
    ),
  ).toBe("MiB/s");
});

test("rateValueAt: converts bytes/s at the base tier", () => {
  expect(rateValueAt(500, "base10", "bytes", 0)).toBe(500);
});

test("rateValueAt: converts bytes/s at a scaled tier", () => {
  expect(rateValueAt(5_000_000, "base10", "bytes", 2)).toBe(5);
});

test("rateValueAt: converts bits/s (8x bytes) at a scaled tier", () => {
  expect(rateValueAt(125, "base10", "bits", 1)).toBe(1);
});

test("niceCeil: passes an already-nice value through unchanged", () => {
  expect(niceCeil(50)).toBe(50);
});

test("niceCeil: rounds an awkward value up to the next 1-2-5 rung", () => {
  expect(niceCeil(73)).toBe(100);
});

test("niceCeil: non-positive input floors at 1", () => {
  expect(niceCeil(0)).toBe(1);
  expect(niceCeil(-5)).toBe(1);
});

test("quantile: empty input has no quantile", () => {
  expect(quantile([], 0.5)).toBeNull();
});

test("quantile: single-value input returns that value regardless of q", () => {
  expect(quantile([5], 0.9)).toBe(5);
});

test("quantile: interpolates between the two straddling positions", () => {
  const sorted = [1, 2, 3, 4, 5];
  expect(quantile(sorted, 0.5)).toBe(3);
  expect(quantile(sorted, 0.4)).toBeCloseTo(2.6);
});

test("niceStep: picks a 1-2-5 rung at or below a small span", () => {
  expect(niceStep(7)).toBe(5);
});

test("niceStep: scales the same ladder up for a large span", () => {
  expect(niceStep(7300)).toBe(5000);
});

test("niceStep: non-positive span floors at 1", () => {
  expect(niceStep(0)).toBe(1);
});

test("niceDomain: empty input falls back to the floor-sized default domain", () => {
  expect(niceDomain([])).toEqual({ min: 0, max: 12, span: 12 });
});

test("niceDomain: widens and snaps a small range", () => {
  expect(niceDomain([10, 12])).toEqual({ min: 0, max: 20, span: 20 });
});

test("niceDomain: widens and snaps a large range", () => {
  expect(niceDomain([100, 900])).toEqual({ min: 0, max: 2000, span: 2000 });
});

test("niceDomain: a zero-width range (min === max) doesn't divide by zero or produce NaN", () => {
  const domain = niceDomain([50, 50]);
  expect(Number.isNaN(domain.min)).toBe(false);
  expect(Number.isNaN(domain.max)).toBe(false);
  expect(Number.isNaN(domain.span)).toBe(false);
  expect(domain).toEqual({ min: 40, max: 60, span: 20 });
});
