import { expect, test } from "bun:test";
import type { LatencyBucket } from "../runner/contract";
import { gaugeLatencyPresentation } from "./gaugeLatency";

function bucket(endT: number, medianRttMs: number): LatencyBucket {
  return {
    t: endT - 100,
    startT: endT - 200,
    endT,
    medianRttMs,
    p95RttMs: medianRttMs,
    maxRttMs: medianRttMs,
    firstRttMs: medianRttMs,
    lastRttMs: medianRttMs,
    rttDeltaSumMs: 0,
    rttDeltaCount: 0,
    pingCount: 1,
    lossCount: 0,
    underLoad: false,
    phase: "latency",
    continuityId: 0,
  };
}

const repeated = (count: number, start: number, median: number) =>
  Array.from({ length: count }, (_, index) =>
    bucket((index + start) * 200, median),
  );

test("pre-bucket fallback derives its scale from the displayed RTT", () => {
  expect(
    gaugeLatencyPresentation({
      phase: "latency",
      liveRttMs: 600,
      liveScaleMs: 20,
      history: [],
      completedRttMs: null,
    }),
  ).toEqual({ rttMs: 600, scaleMs: 1_000 });
});

test("a live bucket keeps the shared recent scale", () => {
  expect(
    gaugeLatencyPresentation({
      phase: "latency",
      liveRttMs: 25,
      liveScaleMs: 40,
      history: [bucket(200, 25)],
      completedRttMs: null,
    }),
  ).toEqual({ rttMs: 25, scaleMs: 40 });
});

test("completed latency uses the full-history chart domain", () => {
  const history = [...repeated(100, 1, 600), ...repeated(80, 101, 10)];

  expect(
    gaugeLatencyPresentation({
      phase: "complete",
      liveRttMs: 10,
      liveScaleMs: 40,
      history,
      completedRttMs: 600,
    }),
  ).toEqual({ rttMs: 600, scaleMs: 1_000 });
});

test("completed fallback derives its scale when history has no RTT", () => {
  const lossOnly = {
    ...bucket(200, 0),
    medianRttMs: null,
    p95RttMs: null,
    maxRttMs: null,
    firstRttMs: null,
    lastRttMs: null,
    pingCount: 1,
    lossCount: 1,
  };

  for (const history of [[], [lossOnly]])
    expect(
      gaugeLatencyPresentation({
        phase: "complete",
        liveRttMs: 0,
        liveScaleMs: 20,
        history,
        completedRttMs: 600,
      }),
    ).toEqual({ rttMs: 600, scaleMs: 1_000 });
});
