import { expect, test } from "bun:test";
import {
  GrowingRateEstimator,
  PRESENTATION_MIN_WINDOW_MS,
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

test("constant rate is exact from the first observation at every cadence", () => {
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
  // At 1.2 s the window is the latest 800 ms: 200 ms at 1k + 600 ms at 2k.
  expect(estimate.presentedBytesPerSec).toBeCloseTo(1_750, 8);
  expect(PRESENTATION_MIN_WINDOW_MS).toBe(800);
});

test("a brief dip cancels without resetting the established regime", () => {
  const estimator = new GrowingRateEstimator();
  for (let i = 0; i < 40; i++) pushRate(estimator, 1_000, 100);
  for (let i = 0; i < 2; i++) pushRate(estimator, 400, 100);
  let estimate = estimator.snapshot();
  for (let i = 0; i < 10; i++) estimate = pushRate(estimator, 1_000, 100);
  expect(estimate.regimeId).toBe(0);
  expect(estimate.candidate).toBeNull();
});

test("sustained downshift confirms from the candidate boundary", () => {
  const estimator = new GrowingRateEstimator();
  for (let i = 0; i < 50; i++) pushRate(estimator, 1_000, 100);
  let changed = false;
  let estimate = estimator.snapshot();
  for (let i = 0; i < 30 && !changed; i++) {
    estimate = pushRate(estimator, 400, 100);
    changed = estimate.regimeChanged;
  }
  expect(changed).toBe(true);
  expect(estimate.regimeAgeMs).toBeGreaterThanOrEqual(
    REGIME_DOWNSHIFT_CONFIRM_MS,
  );
  expect(estimate.presentedBytesPerSec).toBeCloseTo(400, 6);
});

test("sustained recovery confirms as a new upward regime", () => {
  const estimator = new GrowingRateEstimator();
  for (let i = 0; i < 50; i++) pushRate(estimator, 400, 100);
  let estimate = estimator.snapshot();
  for (let i = 0; i < 30 && !estimate.regimeChanged; i++)
    estimate = pushRate(estimator, 1_000, 100);
  expect(estimate.regimeChanged).toBe(true);
  expect(estimate.regimeAgeMs).toBeGreaterThanOrEqual(
    REGIME_UPSHIFT_CONFIRM_MS,
  );
  expect(estimate.presentedBytesPerSec).toBeCloseTo(1_000, 6);
});

test("stall transition is presentation-only and reaches zero exactly", () => {
  expect(GrowingRateEstimator.stallRate(1_000, 0)).toBe(1_000);
  expect(GrowingRateEstimator.stallRate(1_000, STALL_PRESENTATION_MS / 2)).toBe(
    500,
  );
  expect(GrowingRateEstimator.stallRate(1_000, STALL_PRESENTATION_MS)).toBe(0);
  expect(GrowingRateEstimator.stallRate(1_000, STALL_PRESENTATION_MS * 2)).toBe(
    0,
  );
});
