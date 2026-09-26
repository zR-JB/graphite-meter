import { test, expect } from "bun:test";
import {
  chartThroughputScale,
  fixedMs,
  fmtBytes,
  fmtDuration,
  fmtMs,
  fmtMsTick,
  fmtSpeed,
  niceDomain,
  niceStep,
  rateScaleIndex,
  rateUnit,
  rateValueAt,
  throughputUnitIndex,
} from "./format";
import { formatHistoryRate } from "./history/format";

test("durations read in seconds below a minute, then minutes and hours", () => {
  const values = [999, 25_000, 59_900, 60_000, 65_000, 3_599_000, 3_690_000];
  expect(values.map((ms) => fmtDuration(ms))).toEqual([
    "1.0 s",
    "25.0 s",
    "59.9 s",
    "1 min",
    "1 min 5 s",
    "59 min 59 s",
    "1 h 2 min",
  ]);
});

test("byte counts step on powers of 1000 or 1024 with one decimal", () => {
  const cases = [
    [0, "base10", "0 B"],
    [1000, "base10", "1.0 kB"],
    [1500, "base10", "1.5 kB"],
    [1_000_000, "base10", "1.0 MB"],
    [1024, "base2", "1.0 KiB"],
    [1536, "base2", "1.5 KiB"],
    [1024 * 1024, "base2", "1.0 MiB"],
  ] as const;
  expect(cases.map(([bytes, base]) => fmtBytes(bytes, base))).toEqual(
    cases.map(([, , text]) => text),
  );
});

test("rate tiers step at their base, delayed by headroom", () => {
  const cases = [
    [999, "base10", 1, 0],
    [1000, "base10", 1, 1],
    [1_000_000, "base10", 1, 2],
    [1023, "base2", 1, 0],
    [1024, "base2", 1, 1],
    [1199, "base10", 1.2, 0],
    [1200, "base10", 1.2, 1],
  ] as const;
  expect(
    cases.map(([rate, base, headroom]) => rateScaleIndex(rate, base, headroom)),
  ).toEqual(cases.map(([, , , tier]) => tier));
  expect([
    rateValueAt(500, "base10", "bytes", 0),
    rateValueAt(5_000_000, "base10", "bytes", 2),
    rateValueAt(125, "base10", "bits", 1),
  ]).toEqual([500, 5, 1]);
});

test("throughput units promote at 1.2 and start from the automatic reference", () => {
  const gigabit = (value: number) => (value * 1_000_000_000) / 8;
  const cases = [
    [gigabit(0.1), "base10", "bits", "Mbit/s"],
    [gigabit(1.19), "base10", "bits", "Mbit/s"],
    [gigabit(1.2), "base10", "bits", "Gbit/s"],
    [gigabit(10), "base10", "bits", "Gbit/s"],
    [12_500, "base10", "bits", "kbit/s"],
    [chartThroughputScale(0), "base10", "bits", "Mbit/s"],
    [1_200_000, "base10", "bytes", "MB/s"],
    [1_258_292, "base2", "bytes", "MiB/s"],
  ] as const;
  expect(
    cases.map(([rate, base, kind]) =>
      rateUnit(base, kind, throughputUnitIndex(rate, base, kind)),
    ),
  ).toEqual(cases.map(([, , , unit]) => unit));
  expect([fmtSpeed(8.886), fmtSpeed(937)]).toEqual(["8.89", "937.0"]);
});

test("chart axes snap to a 1-2-5 ladder without collapsing a flat range", () => {
  expect([7, 7300, 0].map(niceStep)).toEqual([5, 5000, 1]);
  const ranges = [[], [10, 12], [100, 900], [50, 50]];
  expect(ranges.map((values) => niceDomain(values))).toEqual([
    { min: 0, max: 12, span: 12 },
    { min: 0, max: 20, span: 20 },
    { min: 0, max: 2000, span: 2000 },
    { min: 40, max: 60, span: 20 },
  ]);
});

test("sub-resolution latency reads as below the browser timer resolution", () => {
  expect([0, 0.04, 0.1, 12.34, 250].map(fmtMs)).toEqual([
    "< 0.1",
    "< 0.1",
    "0.1",
    "12.3",
    "250",
  ]);
  expect(fmtMsTick(0)).toBe("0");
});

const vectors: Record<
  "ms" | "speed" | "bytes",
  { in: number; out: string }[]
> & { rate: { bytesPerSec: number; out: string }[] } = await Bun.file(
  new URL("../../../api/format.testvectors.json", import.meta.url),
).json();

test("formatting matches the shared vectors", () => {
  for (const { in: ms, out } of vectors.ms) expect(fixedMs(ms)).toBe(out);
  for (const { in: value, out } of vectors.speed)
    expect(fmtSpeed(value)).toBe(out);
  for (const { in: bytes, out } of vectors.bytes)
    expect(fmtBytes(bytes, "base10")).toBe(out);
  for (const { bytesPerSec, out } of vectors.rate)
    expect(
      formatHistoryRate(bytesPerSec, { base: "base10", kind: "bits" }),
    ).toBe(out);
});
