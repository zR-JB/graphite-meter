import { test, expect } from "bun:test";
import {
  fixedMs,
  fmtAddedMs,
  fmtBytes,
  fmtDuration,
  fmtStageTime,
  parseDuration,
  fmtMs,
  fmtSpeed,
  formatRate,
  rateScaleIndex,
  rateUnit,
  rateValueAt,
  throughputUnitIndex,
} from "./format";

test("durations read in seconds below a minute, then minutes and hours", () => {
  const values = [
    999, 25_000, 59_900, 60_000, 65_000, 3_599_000, 3_690_000, 7_170_000,
    86_399_000,
  ];
  expect(values.map((ms) => fmtDuration(ms))).toEqual([
    "1.0 s",
    "25.0 s",
    "59.9 s",
    "1 min",
    "1 min 5 s",
    "59 min 59 s",
    "1 h 2 min",
    "2 h",
    "24 h",
  ]);
});

test("stage times read as the steppers show them, and every shown time reads back", () => {
  const times = [
    500, 2_500, 4_000, 45_000, 59_900, 90_000, 5_400_000, 7_200_000,
  ];
  const shown = times.map((ms) => fmtStageTime(ms));
  expect(shown).toEqual([
    "0.5 s",
    "2.5 s",
    "4 s",
    "45 s",
    "59.9 s",
    "1 min 30 s",
    "1 h 30 min",
    "2 h",
  ]);
  expect(shown.map(parseDuration)).toEqual(times);
});

test("a typed stage time snaps to the time the stepper shows, in every range", () => {
  const snap = (ms: number) => parseDuration(fmtStageTime(ms))!;
  const typed = [
    0, 450, 2_250, 12_340, 59_940, 59_960, 61_500, 599_990, 3_599_400,
    3_599_700, 5_429_000, 5_431_000, 86_399_000,
  ];
  expect(typed.map(snap)).toEqual([
    0, 500, 2_300, 12_300, 59_900, 60_000, 62_000, 600_000, 3_599_000,
    3_600_000, 5_400_000, 5_460_000, 86_400_000,
  ]);
  // Snapped once, a time shows as it did and stays put.
  for (let ms = 0; ms <= 7_200_000; ms += ms < 120_000 ? 10 : 1_000) {
    const snapped = snap(ms);
    if (fmtStageTime(snapped) !== fmtStageTime(ms) || snap(snapped) !== snapped)
      throw new Error(`${ms} ms snaps to ${snapped} ms`);
  }
});

test("typed stage times are seconds unless they name a unit or read as a clock", () => {
  const typed = {
    "90": 90_000,
    " 2.5 ": 2_500,
    "2h": 7_200_000,
    "2 hours": 7_200_000,
    "1h30m": 5_400_000,
    "3m 20s": 200_000,
    "500ms": 500,
    "1:30": 90_000,
    "1:30:00": 5_400_000,
  };
  for (const [text, ms] of Object.entries(typed))
    expect(parseDuration(text)).toBe(ms);
  for (const text of ["", "abc", "2x", "1:75", "h", "1 fortnight"])
    expect(parseDuration(text)).toBeNull();
});

test("byte counts step on powers of 1000 or 1024 with one decimal", () => {
  const cases = [
    [0, "base10", "0 B"],
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
    [12_500_000, "base10", "bits", "Mbit/s"],
    [1_200_000, "base10", "bytes", "MB/s"],
    [1_258_292, "base2", "bytes", "MiB/s"],
  ] as const;
  expect(
    cases.map(([rate, base, kind]) =>
      rateUnit(base, kind, throughputUnitIndex(rate, base, kind)),
    ),
  ).toEqual(cases.map(([, , , unit]) => unit));
});

const vectors: Record<
  "ms" | "latency" | "speed" | "bytes" | "added",
  { in: number; out: string }[]
> & { rate: { bytesPerSec: number; out: string }[] } = await Bun.file(
  new URL("../../../api/format.testvectors.json", import.meta.url),
).json();

test("formatting matches the shared vectors", () => {
  for (const { in: ms, out } of vectors.ms) expect(fixedMs(ms)).toBe(out);
  for (const { in: ms, out } of vectors.latency) expect(fmtMs(ms)).toBe(out);
  for (const { in: value, out } of vectors.speed)
    expect(fmtSpeed(value)).toBe(out);
  for (const { in: bytes, out } of vectors.bytes)
    expect(fmtBytes(bytes, "base10")).toBe(out);
  for (const { in: ms, out } of vectors.added) expect(fmtAddedMs(ms)).toBe(out);
  for (const { bytesPerSec, out } of vectors.rate)
    expect(formatRate(bytesPerSec, { base: "base10", kind: "bits" })).toBe(out);
  expect(formatRate(1_048_576, { base: "base2", kind: "bytes" })).toBe(
    "1024 KiB/s",
  );
  expect(formatRate(null, { base: "base10", kind: "bits" })).toBe("—");
});

test("no formatter renders a non-finite value as NaN or Infinity", () => {
  for (const bad of [NaN, Infinity, -Infinity])
    for (const text of [
      fmtSpeed(bad),
      fixedMs(bad),
      fmtMs(bad),
      fmtAddedMs(bad),
      fmtDuration(bad),
      fmtBytes(bad, "base10"),
    ])
      expect(text).toBe("—");
});
