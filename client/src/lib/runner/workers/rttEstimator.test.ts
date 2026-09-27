import { test, expect } from "bun:test";
import {
  observeRtt,
  probeDeadline,
  INITIAL_RTT_ESTIMATE,
} from "./rttEstimator";

test("the RTT estimate and probe deadline follow RFC 6298 between a floor and a ceiling", () => {
  const warm = observeRtt(INITIAL_RTT_ESTIMATE, 100);
  expect(warm).toEqual({ srtt: 100, rttvar: 50, haveRtt: true });
  for (const [rtt, srtt, rttvar] of [
    [100, 100, 37.5],
    [300, 125, 87.5],
  ] as const) {
    const next = observeRtt(warm, rtt);
    expect(next.srtt).toBeCloseTo(srtt, 10);
    expect(next.rttvar).toBeCloseTo(rttvar, 10);
  }
  const est = (srtt: number, rttvar: number) => ({
    srtt,
    rttvar,
    haveRtt: true,
  });
  for (const [estimate, floor, deadline] of [
    [INITIAL_RTT_ESTIMATE, 250, 250],
    [est(100, 20), 250, 250],
    [est(100, 20), 50, 180],
    // A perfectly stable link still keeps a 1 ms variance.
    [est(100, 0), 50, 104],
    [est(50_000, 10_000), 250, 10_000],
  ] as const)
    expect(probeDeadline(estimate, 4, floor, 10_000)).toBe(deadline);
});
