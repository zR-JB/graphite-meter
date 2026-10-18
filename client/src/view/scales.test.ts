import { expect, test } from "bun:test";
import type { ThroughputResult } from "../engine/measure/aggregate";
import type { LatencyBucket } from "../engine/measure/buckets";
import { fromRateUnit, type Units } from "./format";
import { latencyAxisMs, latencyCeilingMs, replyTrackMs, throughputScales } from "./scales";

const BITS = { base: "base10", kind: "bits" } as const;
const NONE = { download: null, upload: null, bidirectional: null };
/** One live tick of a single direction in a run still going. */
const tick = (bytesPerSec: number) => ({ lane: bytesPerSec, combined: bytesPerSec, running: true });
const result = (bytesPerSec: number): ThroughputResult => ({
  peakBytesPerSec: bytesPerSec,
  stabilityPct: 100,
  totalBytes: 0,
  reportedBytesPerSec: bytesPerSec,
});

test("each throughput axis holds what it draws, with headroom and no decade jump", () => {
  // One download tick in bytes/s maps to the chart and dial ceilings in bytes/s and the tier. Idle reads 100 Mbit/s.
  const cases = [
    [null, 12.5e6, 125e6, 2],
    [1_000, 1_250, 1_250, 1],
    [100_000, 125_000, 125_000, 1],
    [1e6, 1.25e6, 125e6, 2],
    [12.5e6, 15e6, 125e6, 2],
    [117e6, 125e6, 125e6, 2],
    [125e6, 150e6, 250e6, 2],
    [300e6, 312.5e6, 625e6, 3],
    [1.25e9, 1.5e9, 2.5e9, 3],
  ] as const;
  for (const [rate, chartBytesPerSec, gaugeBytesPerSec, tier] of cases)
    expect(
      throughputScales(NONE, rate === null ? null : tick(rate), "auto", BITS),
      `${rate}`,
    ).toEqual({
      chartBytesPerSec,
      gaugeBytesPerSec,
      tier,
    });
  // The chart draws each bidirectional lane, the dial their sum, live or settled.
  const bidirectional = { chartBytesPerSec: 62.5e6, gaugeBytesPerSec: 125e6, tier: 2 };
  expect(
    throughputScales(NONE, { lane: 50e6, combined: 100e6, running: true }, "auto", BITS),
  ).toEqual(bidirectional);
  const lanes = { ...NONE, bidirectional: { down: result(50e6), up: result(50e6), wire: null } };
  expect(throughputScales(lanes, null, "auto", BITS)).toEqual(bidirectional);
  expect(
    throughputScales({ ...NONE, download: { ...result(6e6), wire: null } }, null, "auto", BITS)
      .chartBytesPerSec,
  ).toBe(6.25e6);
  // A manual maximum is used exactly, not rounded to a step.
  expect(throughputScales(NONE, tick(1e9), 2e6, BITS)).toEqual({
    chartBytesPerSec: 2e6,
    gaugeBytesPerSec: 2e6,
    tier: 2,
  });
});

test("the dial's ceiling is round in the unit the page reads, in each of the four units", () => {
  // The dial reads idle, a tick at 1.1× the giga or gibi prefix (which still reads a prefix below) and a 2 Gbit/s tick
  // as 1, 2 and 5 Gbit/s; 200 MB/s, 2000 MB/s, 500 MB/s; 1000 Mibit/s, 2000 Mibit/s, 2 Gibit/s; 200, 2000, 500 MiB/s.
  const all: [Units, number, number, number][] = [
    [{ base: "base10", kind: "bits" }, 125e6, 250e6, 625e6],
    [{ base: "base10", kind: "bytes" }, 200e6, 2e9, 500e6],
    [{ base: "base2", kind: "bits" }, 1000 * 2 ** 17, 2000 * 2 ** 17, 2 ** 28],
    [{ base: "base2", kind: "bytes" }, 200 * 2 ** 20, 2000 * 2 ** 20, 500 * 2 ** 20],
  ];
  for (const [units, idle, band, busy] of all)
    expect(
      [null, tick(fromRateUnit(1.1, units, 3)), tick(250e6)].map(
        (live) => throughputScales(NONE, live, "auto", units).gaugeBytesPerSec,
      ),
      `${units.base} ${units.kind}`,
    ).toEqual([idle, band, busy]);
});

test("a stage settling never rescales the page; a finished run reads in the tier History gives it", () => {
  // Each case gives a live peak and the result it settled at, in bytes/s, the tier and dial through the run, and the
  // run's tier once it ends, History's when a result settled, else the one its live figures were drawn in.
  const cases = [
    // 1.3 Mbit/s live, 1.1 settled.
    [162_500, 137_500, 2, 125e6, 1],
    // 800 kbit/s ticks, then the stage failed.
    [100_000, null, 1, 125_000, 1],
    // 1.25 Gbit/s live, 1.15 settled.
    [156.25e6, 143.75e6, 3, 250e6, 2],
  ] as const;
  for (const [peak, settled, tier, gaugeBytesPerSec, finishedTier] of cases) {
    const results = {
      ...NONE,
      download: settled === null ? null : { ...result(settled), wire: null },
    };
    const running = throughputScales(NONE, tick(peak), "auto", BITS);
    expect([running.tier, running.gaugeBytesPerSec], `${peak}`).toEqual([tier, gaugeBytesPerSec]);
    expect(throughputScales(results, tick(peak), "auto", BITS), `${peak} settled`).toEqual(running);
    const finished = throughputScales(results, { ...tick(peak), running: false }, "auto", BITS);
    expect(finished.tier, `${peak} finished`).toBe(finishedTier);
    if (settled === null) continue;
    const history = throughputScales(results, null, "auto", BITS);
    expect(history.tier, `${peak} in History`).toBe(finishedTier);
    expect(finished.gaugeBytesPerSec).toBe(history.gaugeBytesPerSec);
  }
});

test("a corrupt saved or live rate cannot overflow an axis", () => {
  const top = { chartBytesPerSec: 1e15, gaugeBytesPerSec: 1.25e15, tier: 4 };
  for (const rate of [1e308, Number.MAX_VALUE]) {
    const saved = { ...NONE, download: { ...result(rate), wire: null } };
    expect(throughputScales(saved, null, "auto", BITS), `${rate}`).toEqual(top);
    expect(
      throughputScales(saved, { ...tick(rate), running: false }, "auto", BITS),
      `${rate}`,
    ).toEqual(top);
  }
  expect(throughputScales(NONE, tick(Infinity), "auto", BITS)).toEqual(top);
  const idle = throughputScales(NONE, null, "auto", BITS);
  expect(throughputScales(NONE, tick(NaN), "auto", BITS)).toEqual(idle);
  const negative = {
    download: { ...result(-5), wire: null },
    upload: { ...result(-5), wire: null },
    bidirectional: { down: result(-5), up: result(-5), wire: null },
  };
  expect(throughputScales(negative, null, "auto", BITS)).toEqual(idle);
});

test("latency axes step 1, 2, 4, 10 over their percentile, live over the last 8 s", () => {
  const ceilings = [
    [[], 20],
    [[0], 1],
    [[0.1], 1],
    [[3], 4],
    [[3.3], 10],
    [[16], 20],
    [[null, 30], 40],
    [[5_000], 10_000],
  ] as const;
  for (const [values, ms] of ceilings) expect(latencyCeilingMs(values), `${values}`).toBe(ms);
  const tracks = [
    [[], 20],
    [[0], 0.1],
    [[0.001], 0.1],
    [[0.05], 0.2],
    [[0.1], 0.4],
    [[3], 10],
    [[30], 100],
  ] as const;
  for (const [values, ms] of tracks) expect(replyTrackMs(values), `${values}`).toBe(ms);
  // Ten buckets at 100 ms, then ninety at 10 ms, one every 100 ms.
  const buckets: LatencyBucket[] = [...Array(10).fill(100), ...Array(90).fill(10)].map((ms, i) => ({
    phase: "latency",
    continuityId: 0,
    startT: i * 100 - 100,
    endT: i * 100,
    medianRttMs: ms,
    minRttMs: ms,
    p95RttMs: ms,
    maxRttMs: ms,
    pingCount: 1,
    timeoutCount: 0,
  }));
  expect(latencyAxisMs(buckets.slice(0, 80), false)).toBe(200);
  expect(latencyAxisMs(buckets, false)).toBe(20);
  expect(latencyAxisMs(buckets, true)).toBe(200);
});
