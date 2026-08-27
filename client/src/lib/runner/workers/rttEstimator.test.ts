import { test, expect } from "bun:test";
import { observeRtt, lossTimeout, INITIAL_RTT_ESTIMATE } from "./rttEstimator";

test("observeRtt: the first sample seeds srtt directly and rttvar to half of it", () => {
  const est = observeRtt(INITIAL_RTT_ESTIMATE, 100);
  expect(est).toEqual({ srtt: 100, rttvar: 50, haveRtt: true });
});

test("observeRtt: a repeated identical RTT holds srtt and decays rttvar", () => {
  const first = observeRtt(INITIAL_RTT_ESTIMATE, 100);
  const second = observeRtt(first, 100); // same RTT again: srtt unchanged; 0.875*100 + 0.125*100 = 100.
  expect(second.srtt).toBeCloseTo(100, 10);
  // rttvar: 0.75*50 + 0.25*|100-100| = 37.5 (decays toward 0 over repeats)
  expect(second.rttvar).toBeCloseTo(37.5, 10);
});

test("observeRtt: an RTT jump moves srtt slowly but spikes rttvar immediately", () => {
  const warm = observeRtt(INITIAL_RTT_ESTIMATE, 100);
  const jump = observeRtt(warm, 300);
  // srtt: 0.875*100 + 0.125*300 = 125
  expect(jump.srtt).toBeCloseTo(125, 10);
  // rttvar: 0.75*50 + 0.25*|100-300| = 87.5
  expect(jump.rttvar).toBeCloseTo(87.5, 10);
});

test("lossTimeout: before any sample, the floor governs (cold start)", () => {
  expect(lossTimeout(INITIAL_RTT_ESTIMATE, 4, 250, 10_000)).toBe(250);
});

test("lossTimeout: RTO = srtt + k*rttvar once warmed", () => {
  const est = { srtt: 100, rttvar: 20, haveRtt: true };
  expect(lossTimeout(est, 4, 250, 10_000)).toBe(250); // 180 < floor(250)
  expect(lossTimeout(est, 4, 50, 10_000)).toBe(180); // 100 + 4*20
});

test("lossTimeout: rttvar is floored at 1 so a perfectly stable link still has margin", () => {
  const est = { srtt: 100, rttvar: 0, haveRtt: true };
  expect(lossTimeout(est, 4, 50, 10_000)).toBe(104); // 100 + 4*max(1,0)
});

test("lossTimeout: clamps at the ceiling on a pathologically slow/jittery link", () => {
  const est = { srtt: 50_000, rttvar: 10_000, haveRtt: true };
  expect(lossTimeout(est, 4, 250, 10_000)).toBe(10_000);
});
