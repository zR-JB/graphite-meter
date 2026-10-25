import { expect, test } from "bun:test";
import { json } from "../protocol/pins.testutil";
import * as format from "./format";
import {
  fmtAddedMs,
  fmtBytes,
  fmtCount,
  fmtCounted,
  fmtDuration,
  fmtLatency,
  fmtLatencyRange,
  fmtMs,
  fmtPercent,
  fmtProgress,
  fmtRate,
  fmtSpeed,
  fmtStageTime,
  fmtTick,
  MISSING,
  parseDuration,
  rateTier,
  rateUnit,
} from "./format";

const BITS = { base: "base10", kind: "bits" } as const;

test("numbers read as the vectors the terminal clients share", async () => {
  const vectors = await json("format.testvectors.json");
  // The fixed-ms figure is the added figure's magnitude.
  for (const { in: ms, out } of vectors.ms) expect(fmtAddedMs(ms)).toBe(`+${out}`);
  for (const { in: ms, out } of vectors.latency) expect(fmtMs(ms)).toBe(out);
  for (const { in: value, out } of vectors.speed) expect(fmtSpeed(value)).toBe(out);
  for (const { bytesPerSec, out } of vectors.rate)
    expect(fmtRate(bytesPerSec, { base: "base10", kind: "bits" })).toBe(out);
  for (const { in: bytes, out } of vectors.bytes) expect(fmtBytes(bytes, "base10")).toBe(out);
  for (const { in: ms, out } of vectors.added) expect(fmtAddedMs(ms)).toBe(out);
  for (const { ms, out } of vectors.duration) expect(fmtDuration(ms), `${ms}`).toBe(out);
  for (const { ms, out } of vectors.stageTime) expect(fmtStageTime(ms), `${ms}`).toBe(out);
  for (const { elapsedMs, totalMs, out } of vectors.progress)
    expect(fmtProgress(elapsedMs, totalMs), `${elapsedMs}`).toBe(out);
  for (const { fastestMs, slowestMs, out } of vectors.latencyRange)
    expect(fmtLatencyRange(fastestMs, slowestMs)).toBe(out);
  for (const { ratio, out } of vectors.percent)
    expect(fmtPercent(ratio * 100), `${ratio}`).toBe(out);
  for (const { in: count, out } of vectors.count) expect(fmtCount(count)).toBe(out);
  for (const { in: value, out } of vectors.tick) expect(fmtTick(value), `${value}`).toBe(out);
  expect(fmtRate(1_048_576, { base: "base2", kind: "bytes" })).toBe("1024 KiB/s");
});

test("every formatter reads a missing or non-finite value as a bare dash", () => {
  const units = { base: "base10", kind: "bits" } as const;
  const formatters: Record<string, (bad: number | null | undefined) => string[]> = {
    fmtSpeed: (bad) => [format.fmtSpeed(bad)],
    fmtMs: (bad) => [format.fmtMs(bad)],
    fmtLatency: (bad) => [format.fmtLatency(bad)],
    fmtLatencyRange: (bad) => [format.fmtLatencyRange(bad, 1), format.fmtLatencyRange(1, bad)],
    fmtAddedMs: (bad) => [format.fmtAddedMs(bad)],
    fmtBytes: (bad) => [format.fmtBytes(bad, "base2")],
    fmtRate: (bad) => [format.fmtRate(bad, units), format.fmtRate(bad, { ...units, tier: 2 })],
    fmtDuration: (bad) => [format.fmtDuration(bad), format.fmtDuration(bad, 0)],
    fmtStageTime: (bad) => [format.fmtStageTime(bad)],
    fmtProgress: (bad) => [format.fmtProgress(bad, 4_000), format.fmtProgress(1_000, bad)],
    fmtCount: (bad) => [format.fmtCount(bad)],
    fmtCounted: (bad) => [format.fmtCounted(bad, "reply", "replies")],
    fmtPercent: (bad) => [format.fmtPercent(bad)],
    fmtTick: (bad) => [format.fmtTick(bad)],
  };
  expect(Object.keys(formatters).sort()).toEqual(
    Object.keys(format)
      .filter((name) => name.startsWith("fmt"))
      .sort(),
  );
  for (const bad of [NaN, Infinity, -Infinity, null, undefined]) {
    for (const [name, texts] of Object.entries(formatters))
      expect(texts(bad), name).toEqual(texts(bad).map(() => MISSING));
    expect(format.rateReading(bad, units)).toEqual({ figure: MISSING, unit: "" });
  }
});

test("a prefix is promoted only from 1.2× its step, as the terminal clients judge it", () => {
  const gigabit = (value: number) => (value * 1e9) / 8;
  const cases = [
    [0, "base10", "bits", "bit/s"],
    [12_500, "base10", "bits", "kbit/s"],
    [12_500_000, "base10", "bits", "Mbit/s"],
    [gigabit(0.1), "base10", "bits", "Mbit/s"],
    [gigabit(1.19), "base10", "bits", "Mbit/s"],
    [gigabit(1.1994), "base10", "bits", "Mbit/s"],
    [gigabit(1.1996), "base10", "bits", "Mbit/s"],
    [gigabit(1.2), "base10", "bits", "Gbit/s"],
    [1_199, "base10", "bytes", "B/s"],
    [1_200, "base10", "bytes", "kB/s"],
    [1_200_000, "base10", "bytes", "MB/s"],
    [1_258_291, "base2", "bytes", "KiB/s"],
    [1_258_292, "base2", "bytes", "MiB/s"],
    [1e16, "base2", "bits", "Tibit/s"],
  ] as const;
  for (const [bytesPerSec, base, kind, unit] of cases)
    expect(rateUnit({ base, kind }, rateTier(bytesPerSec, { base, kind })), `${bytesPerSec}`).toBe(
      unit,
    );
  // A page that sets a tier reads every rate in it, zero included.
  expect(fmtRate(0, { base: "base10", kind: "bits", tier: 2 })).toBe("0.00 Mbit/s");
  expect(fmtRate(gigabit(1.1), { base: "base10", kind: "bits" })).toBe("1100 Mbit/s");
  // The terminal clients judge the tier before rounding. The browser reads the same.
  expect([gigabit(1.1996), gigabit(1.2)].map((rate) => fmtRate(rate, BITS))).toEqual([
    "1200 Mbit/s",
    "1.20 Gbit/s",
  ]);
});

test("durations in whole seconds round to a minute as soon as they read 60 s", () => {
  const cases = [
    [999, "1 s"],
    [59_499, "59 s"],
    [59_949, "1 min"],
    [3_599_600, "1 h"],
    [-5, "0 s"],
  ] as const;
  for (const [ms, whole] of cases) expect(fmtDuration(ms, 0), `${ms}`).toBe(whole);
  expect(fmtDuration(-5)).toBe("0.0 s");
});

test("a typed stage time snaps to the time the stepper shows, and stays there", async () => {
  const shown = (await json("format.testvectors.json")).stageTime;
  for (const { out } of shown) expect(fmtStageTime(parseDuration(out)), out).toBe(out);
  const snap = (ms: number) => parseDuration(fmtStageTime(ms))!;
  const typed = [
    0, 450, 2_250, 12_340, 59_940, 59_960, 61_500, 599_990, 3_599_400, 3_599_700, 5_429_000,
    86_399_000,
  ];
  expect(typed.map(snap)).toEqual([
    0, 500, 2_300, 12_300, 59_900, 60_000, 62_000, 600_000, 3_599_000, 3_600_000, 5_400_000,
    86_400_000,
  ]);
  for (let ms = 0; ms <= 86_400_000; ms += ms < 120_000 ? 10 : ms < 7_200_000 ? 1_000 : 7_000) {
    const snapped = snap(ms);
    if (fmtStageTime(snapped) !== fmtStageTime(ms) || snap(snapped) !== snapped)
      throw new Error(`${ms} ms snaps to ${snapped} ms`);
  }
});

test("typed times are seconds unless they name a unit or read as a clock", () => {
  const typed = {
    "90": 90_000,
    " 2.5 ": 2_500,
    "  2H ": 7_200_000,
    "2 hours": 7_200_000,
    "1h30m": 5_400_000,
    "3 m 20": 200_000,
    "5min30s": 330_000,
    "1m 1ms": 60_001,
    "1.5m": 90_000,
    "1:5": 65_000,
    "1:30:00": 5_400_000,
    "1 2": 3_000,
  };
  for (const [text, ms] of Object.entries(typed)) expect(parseDuration(text), text).toBe(ms);
  const unreadable = [
    "",
    "abc",
    "h",
    "2x",
    "1 fortnight",
    ".5",
    "1.",
    "1.5.5",
    "-5",
    "1e3",
    "1:75",
    "1:30:00:00",
  ];
  for (const text of [...unreadable, `${"9".repeat(400)}:00`])
    expect(parseDuration(text), text).toBeNull();
});

test("progress from before its start, and counts with a fraction, read as documented", () => {
  expect(fmtProgress(-5, 90_000)).toBe("0.0 / 1 min 30 s");
  const counted = [0, 1, 2, 1_204, 1_234_567.891].map((count) =>
    fmtCounted(count, "reply", "replies"),
  );
  expect(counted).toEqual([
    "0 replies",
    "1 reply",
    "2 replies",
    "1,204 replies",
    "1,234,567.891 replies",
  ]);
  expect(fmtCounted(3, "server")).toBe("3 servers");
});

test("no figure reads as a negative zero or in exponent form", () => {
  const zeros = [
    fmtSpeed(-0.001),
    fmtRate(-0.0001, BITS),
    fmtLatency(-0.01),
    fmtBytes(-0.4, "base10"),
    fmtTick(-1e-200),
    fmtCount(-0.0001),
    fmtPercent(-0.001),
    fmtAddedMs(-0.04),
  ];
  expect(zeros).toEqual(["0.00", "0.00 bit/s", "0.0 ms", "0 B", "0", "0", "0.0%", "+0.0"]);
  expect([fmtSpeed(1e21), fmtTick(1e21), fmtSpeed(-1e23), fmtMs(1e22)]).toEqual([
    "1000000000000000000000",
    "1000000000000000000000",
    "-100000000000000000000000",
    "10000000000000000000000",
  ]);
});
