import { expect, test } from "bun:test";
import { RunAccumulator } from "./evaluation";
import {
  GrowingRateEstimator,
  PRESENTATION_MIN_WINDOW_MS,
  presentationWindowMs,
  REGIME_DOWNSHIFT_CONFIRM_MS,
  REGIME_UPSHIFT_CONFIRM_MS,
  STALL_PRESENTATION_MS,
} from "./rateEstimator";
function pushRate(
  estimator: GrowingRateEstimator,
  bytesPerSec: number,
  durationMs: number,
) {
  return estimator.observe({
    bytes: (bytesPerSec * durationMs) / 1_000,
    durationMs,
  });
}
function boundaryCount(
  rates: number[],
  durations: number[] = Array(rates.length).fill(100),
): number {
  const estimator = new GrowingRateEstimator();
  let boundaries = 0;
  for (let index = 0; index < rates.length; index++) {
    if (pushRate(estimator, rates[index], durations[index]).regimeChanged)
      boundaries++;
  }
  return boundaries;
}
test("stationary low-noise is exact from the first observation at every cadence", () => {
  for (const cadence of [20, 60, 100, 137]) {
    const estimator = new GrowingRateEstimator();
    let estimate = pushRate(estimator, 75_000_000, cadence);
    for (let elapsed = cadence; elapsed < 10_000; elapsed += cadence)
      estimate = pushRate(
        estimator,
        75_000_000,
        Math.min(cadence, 10_000 - elapsed),
      );
    expect(estimate.presentedBytesPerSec).toBeCloseTo(75_000_000, 4);
    expect(estimate.regimeAgeMs).toBe(10_000);
  }
});
test("window boundary prorates observations", () => {
  const estimator = new GrowingRateEstimator();
  pushRate(estimator, 1_000, 600);
  const estimate = pushRate(estimator, 2_000, 600);
  expect(estimate.presentedBytesPerSec).toBeCloseTo(1_620 / 1.02, 8);
  expect(PRESENTATION_MIN_WINDOW_MS).toBe(800);
});
test("stationary low-noise uses the 85% current-regime evidence window", () => {
  expect(presentationWindowMs(400)).toBe(400);
  expect(presentationWindowMs(800)).toBe(800);
  expect(presentationWindowMs(1_200)).toBe(1_020);
  expect(presentationWindowMs(10_000)).toBe(8_500);
});
test("stationary low-noise, stationary high-noise, bursty stationary, autocorrelated noise, periodic dips, and irregular callback cadence create no boundaries", () => {
  const stationaryLowNoise = Array.from(
    { length: 100 },
    (_, i) => 1_000 * (1 + [0, 0.015, -0.01, 0.02, -0.015][i % 5]),
  );
  const stationaryHighNoise = Array.from(
    { length: 100 },
    (_, i) => 1_000 * (1 + [0.1, -0.08, 0.12, -0.1, 0.04, -0.06][i % 6]),
  );
  const burstyStationary = Array.from({ length: 100 }, (_, i) =>
    i % 10 === 0 ? 1_400 : 1_000,
  );
  const autocorrelatedNoise = Array.from(
    { length: 100 },
    (_, i) => 1_000 + Math.sin(i / 4) * 140,
  );
  const periodicDips = Array.from({ length: 100 }, (_, i) =>
    i % 20 === 0 || i % 20 === 1 ? 450 : 1_000,
  );
  const irregularCadence = [20, 60, 137, 100, 43, 240, 80, 120];
  for (const trace of [
    stationaryLowNoise,
    stationaryHighNoise,
    burstyStationary,
    autocorrelatedNoise,
    periodicDips,
  ])
    expect(boundaryCount(trace)).toBe(0);
  expect(
    boundaryCount(
      Array(irregularCadence.length * 12).fill(1_000),
      Array.from(
        { length: irregularCadence.length * 12 },
        (_, i) => irregularCadence[i % irregularCadence.length],
      ),
    ),
  ).toBe(0);
});
test("short transient drop cancels without resetting the established regime", () => {
  expect(
    boundaryCount([
      ...Array(40).fill(1_000),
      400,
      400,
      ...Array(20).fill(1_000),
    ]),
  ).toBe(0);
});
test.each([
  {
    label:
      "sustained downward step confirms once from the candidate-establishing observation",
    from: 1_000,
    to: 400,
    confirmationMs: REGIME_DOWNSHIFT_CONFIRM_MS,
  },
  {
    label:
      "sustained upward step confirms once and settles after the evidence floor",
    from: 400,
    to: 1_000,
    confirmationMs: REGIME_UPSHIFT_CONFIRM_MS,
  },
])("$label", ({ from, to, confirmationMs }) => {
  const estimator = new GrowingRateEstimator();
  for (let i = 0; i < 50; i++) pushRate(estimator, from, 100);
  let boundaries = 0;
  let estimate = estimator.snapshot();
  for (let i = 0; i < 30; i++) {
    estimate = pushRate(estimator, to, 100);
    if (estimate.regimeChanged) boundaries++;
  }
  expect(boundaries).toBe(1);
  expect(estimate.regimeAgeMs).toBeGreaterThanOrEqual(confirmationMs);
  expect(estimate.presentedBytesPerSec).toBeCloseTo(to, 6);
});
test("gradual ramp does not repeatedly reset the estimator", () => {
  const gradualRamp = Array.from(
    { length: 140 },
    (_, i) => 1_000 + Math.min(i, 100) * 6,
  );
  expect(boundaryCount(gradualRamp)).toBeLessThanOrEqual(1);
});
test("irregular callback cadence preserves equivalent presentation and final reduction", () => {
  const evidence = [
    { rate: 1_000, durationMs: 1_000 },
    { rate: 2_000, durationMs: 2_000 },
  ];
  const reduce = (cadenceMs: number): { presented: number; final: number } => {
    const estimator = new GrowingRateEstimator();
    const accumulator = new RunAccumulator();
    for (const segment of evidence) {
      for (
        let elapsed = 0;
        elapsed < segment.durationMs;
        elapsed += cadenceMs
      ) {
        const durationMs = Math.min(cadenceMs, segment.durationMs - elapsed);
        const bytes = (segment.rate * durationMs) / 1_000;
        pushRate(estimator, segment.rate, durationMs);
        accumulator.pushThroughput(
          "download",
          "down",
          bytes,
          durationMs / 1_000,
        );
      }
    }
    return {
      presented: estimator.snapshot().presentedBytesPerSec,
      final: accumulator.throughputResult("download", false)
        .reportedBytesPerSec,
    };
  };
  const natural = reduce(100);
  const irregular = reduce(137);
  expect(irregular.presented).toBeCloseTo(natural.presented, 8);
  expect(irregular.final).toBeCloseTo(natural.final, 8);
  expect(natural.final).toBeCloseTo(5_000 / 3, 8);
});
test("stall + recovery transition is presentation-only and reaches zero exactly", () => {
  expect(GrowingRateEstimator.stallRate(1_000, 0)).toBe(1_000);
  expect(GrowingRateEstimator.stallRate(1_000, STALL_PRESENTATION_MS / 2)).toBe(
    500,
  );
  expect(GrowingRateEstimator.stallRate(1_000, STALL_PRESENTATION_MS)).toBe(0);
  expect(GrowingRateEstimator.stallRate(1_000, STALL_PRESENTATION_MS * 2)).toBe(
    0,
  );
});
