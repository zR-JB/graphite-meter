import { expect, test } from "bun:test";
import {
  GrowingRateEstimator,
  LiveRates,
  presentationWindowMs,
} from "./liveRates";

const push = (estimator: GrowingRateEstimator, rate: number, ms = 100) =>
  estimator.observe((rate * ms) / 1_000, ms);

function boundaries(rates: number[], durations?: number[]): number {
  const estimator = new GrowingRateEstimator();
  return rates.filter((rate, i) => push(estimator, rate, durations?.[i] ?? 100))
    .length;
}

test("a stationary rate is exact from the first observation at every cadence", () => {
  for (const cadence of [20, 60, 100, 137]) {
    const estimator = new GrowingRateEstimator();
    for (let elapsed = 0; elapsed < 10_000; elapsed += cadence)
      push(estimator, 75_000_000, Math.min(cadence, 10_000 - elapsed));
    expect(estimator.presented).toBeCloseTo(75_000_000, 4);
  }
  const prorated = new GrowingRateEstimator();
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

test.each([
  [1_000, 400],
  [400, 1_000],
])("a sustained step from %d to %d confirms once and settles", (from, to) => {
  const estimator = new GrowingRateEstimator();
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
  live.download("a", 1_000, 1_000);
  live.download("b", 3_000, 1_000);
  expect(live.rate("down")).toBe(4_000);

  const frame = (at: number, bytes: number) => ({
    id: "u",
    bytes,
    nanos: at * 1e6,
    receivedAtMs: at,
  });
  for (let at = 0; at <= 400; at += 100)
    live.receiver("a", frame(at, at * 10), at);
  expect(live.rate("up")).toBeCloseTo(10_000, 6);
  const lanes = () => 2;
  // Regular arrivals need no bridge.
  expect(live.bridgedUpload(450, lanes)).toBeNull();
  live.hint("a", 0, 7_000, 1_000, 900);
  expect(live.bridgedUpload(950, lanes)).toBeNull();
  live.hint("a", 1, 8_000, 1_000, 950);
  // A long pause with fresh hints from both lanes bounds the estimate to +25%.
  expect(live.bridgedUpload(1_000, lanes)).toBeCloseTo(12_500, 6);
  // Stale hints return the display to the receiver's rate.
  expect(live.bridgedUpload(1_300, lanes)).toBeNull();
  live.receiver("a", frame(1_400, 14_000), 1_400);
  live.hint("a", 0, 1_000, 1_000, 1_400);
  live.hint("a", 1, 1_000, 1_000, 1_400);
  expect(live.bridgedUpload(1_450, lanes)).toBeNull();
});
