import { expect, test } from "bun:test";
import {
  GrowingRateEstimator,
  LiveRates,
  presentationWindowMs,
  PRESENTATION_FIRST_MS,
} from "./liveRates";

const push = (estimator: GrowingRateEstimator, rate: number, ms = 100) =>
  estimator.observe((rate * ms) / 1_000, ms);

// The first delivery only anchors an estimator.
const anchored = () => {
  const estimator = new GrowingRateEstimator();
  estimator.observe(1, 1);
  return estimator;
};

function boundaries(rates: number[], durations?: number[]): number {
  const estimator = anchored();
  return rates.filter((rate, i) => push(estimator, rate, durations?.[i] ?? 100))
    .length;
}

test("a stationary rate is exact once presented, at every cadence", () => {
  for (const cadence of [20, 60, 100, 137]) {
    const estimator = anchored();
    for (let elapsed = 0; elapsed < 10_000; elapsed += cadence)
      push(estimator, 75_000_000, Math.min(cadence, 10_000 - elapsed));
    expect(estimator.presented).toBeCloseTo(75_000_000, 4);
  }
  const prorated = anchored();
  push(prorated, 1_000, 600);
  push(prorated, 2_000, 600);
  expect(prorated.presented).toBeCloseTo(1_620 / 1.02, 8);
  expect(presentationWindowMs(400)).toBe(400);
  expect(presentationWindowMs(1_200)).toBe(1_020);
  expect(presentationWindowMs(10_000)).toBe(8_500);
});

test("noise, bursts, dips, ramps and transient drops keep one regime", () => {
  const traces = [
    (i: number) => 1_000 * (1 + [0, 0.015, -0.01, 0.02, -0.015][i % 5]),
    (i: number) => 1_000 * (1 + [0.1, -0.08, 0.12, -0.1, 0.04, -0.06][i % 6]),
    (i: number) => (i % 10 === 0 ? 1_400 : 1_000),
    (i: number) => 1_000 + Math.sin(i / 4) * 140,
    (i: number) => (i % 20 < 2 ? 450 : 1_000),
  ];
  for (const trace of traces)
    expect(boundaries(Array.from({ length: 100 }, (_, i) => trace(i)))).toBe(0);
  const cadence = [20, 60, 137, 100, 43, 240, 80, 120];
  expect(
    boundaries(
      Array(96).fill(1_000),
      Array.from({ length: 96 }, (_, i) => cadence[i % cadence.length]),
    ),
  ).toBe(0);
  const drop = [...Array(40).fill(1_000), 400, 400, ...Array(20).fill(1_000)];
  expect(boundaries(drop)).toBe(0);
  const ramp = Array.from(
    { length: 140 },
    (_, i) => 1_000 + Math.min(i, 100) * 6,
  );
  expect(boundaries(ramp)).toBeLessThanOrEqual(1);
});

test("long irregular traces preserve fractional window accounting after pruning and reset", () => {
  const estimator = anchored();
  const spans: { start: number; end: number; bytes: number }[] = [];
  let elapsed = 0;
  for (let i = 0; i < 5_000; i++) {
    const ms = [20, 137, 60, 43, 240][i % 5];
    const bytes = (75_000_000 * (1 + ((i % 7) - 3) / 100) * ms) / 1000;
    spans.push({ start: elapsed, end: elapsed + ms, bytes });
    elapsed += ms;
    expect(estimator.observe(bytes, ms)).toBe(false);
    if (i % 137 !== 0 || elapsed < PRESENTATION_FIRST_MS) continue;
    const from = elapsed - presentationWindowMs(elapsed);
    let total = 0;
    for (const span of spans)
      if (span.end > from)
        total +=
          span.bytes *
          ((span.end - Math.max(from, span.start)) / (span.end - span.start));
    expect(
      estimator.presented / ((total * 1000) / (elapsed - from)),
    ).toBeCloseTo(1, 10);
  }
  // A downshift discards most old evidence and compacts the retained window.
  for (let i = 0; i < 40; i++) push(estimator, 1_000);
  expect(estimator.presented).toBeCloseTo(1_000, 5);
  estimator.reset();
  push(estimator, 2_000, 137);
  push(estimator, 2_000, 499);
  expect(estimator.presented).toBe(0);
  push(estimator, 2_000, 1);
  expect(estimator.presented).toBeCloseTo(2_000, 8);
});

test.each([
  [1_000, 400],
  [400, 1_000],
])("a sustained step from %d to %d confirms once and settles", (from, to) => {
  const estimator = anchored();
  for (let i = 0; i < 50; i++) push(estimator, from);
  let confirmed = 0;
  for (let i = 0; i < 30; i++) if (push(estimator, to)) confirmed++;
  expect(confirmed).toBe(1);
  expect(estimator.presented).toBeCloseTo(to, 6);
});

test("servers present independently, and only an irregular receiver is bridged by every lane's fresh hint", () => {
  const live = new LiveRates();
  live.reset({ a: 0, b: 0 }, 0);
  expect(live.rate("down")).toBeNull();
  live.download("a", 500, 500);
  live.download("b", 1_500, 500);
  expect(live.rate("down")).toBeNull();
  live.download("a", 1_500, 1_500);
  live.download("b", 4_500, 1_500);
  expect(live.rate("down")).toBe(4_000);

  const frame = (at: number, bytes: number) => ({
    id: "u",
    bytes,
    nanos: at * 1e6,
    receivedAtMs: at,
  });
  for (let at = 0; at <= 700; at += 100)
    live.receiver("a", frame(at, at * 10), at);
  expect(live.rate("up")).toBeCloseTo(10_000, 6);
  const lanes = () => 2;
  // Regular arrivals need no bridge.
  expect(live.bridgedUpload(750, lanes)).toBeNull();
  live.hint("a", 0, 7_000, 1_000, 1_200);
  expect(live.bridgedUpload(1_250, lanes)).toBeNull();
  live.hint("a", 1, 8_000, 1_000, 1_250);
  // A long pause with fresh hints from both lanes bounds the estimate to +25%.
  expect(live.bridgedUpload(1_300, lanes)).toBeCloseTo(12_500, 6);
  // Stale hints return the display to the receiver's rate.
  expect(live.bridgedUpload(1_600, lanes)).toBeNull();
  live.receiver("a", frame(1_700, 17_000), 1_700);
  live.hint("a", 0, 1_000, 1_000, 1_700);
  live.hint("a", 1, 1_000, 1_000, 1_700);
  expect(live.bridgedUpload(1_750, lanes)).toBeNull();
});

test("a slow receiver counting 64 KiB reads never opens with a peak", () => {
  // 9.3 Mbit/s into a server that counts whole reads, a feed record every 100 ms and the boundary's checkpoint 2 ms
  // after the first record, with a read completing between them: that pair alone once showed 28 times the rate.
  const rate = 1_162_500;
  const read = 65_536;
  const counted = (ms: number) =>
    Math.floor((rate * (ms + 55)) / 1000 / read) * read;
  const live = new LiveRates();
  live.reset({ s: 0 }, 0);
  const shown: number[] = [];
  for (const at of [
    0,
    2,
    ...Array.from({ length: 60 }, (_, i) => 100 * (i + 1)),
  ]) {
    live.receiver(
      "s",
      { id: "u", bytes: counted(at), nanos: at * 1e6, receivedAtMs: at },
      at,
    );
    const value = live.rate("up");
    if (value !== null) shown.push(value);
  }
  expect(shown.length).toBeGreaterThan(50);
  expect(Math.max(...shown) / rate).toBeLessThan(1.15);
  expect(shown.at(-1)! / rate).toBeCloseTo(1, 1);
});

test("the estimator matches the vectors the terminal clients share", async () => {
  const { cases } = await Bun.file(
    new URL("../../../../api/liverate.testvectors.json", import.meta.url),
  ).json();
  for (const { name, steps } of cases as {
    name: string;
    steps: [number, number, number, boolean][];
  }[]) {
    const estimator = new GrowingRateEstimator();
    for (const [bytes, ms, presented, changed] of steps) {
      expect(estimator.observe(bytes, ms), name).toBe(changed);
      expect(estimator.presented, name).toBe(presented);
    }
  }
});
