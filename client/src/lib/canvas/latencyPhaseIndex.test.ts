import { expect, test } from "bun:test";
import type { LatencyBucket, Phase } from "../runner/contract";
import { LatencyPhaseIndex } from "./latencyPhaseIndex";

function bucket(
  startT: number,
  phase: Phase,
  medianRttMs: number,
): LatencyBucket {
  return {
    t: startT + 100,
    startT,
    endT: startT + 200,
    medianRttMs,
    p95RttMs: medianRttMs,
    maxRttMs: medianRttMs,
    firstRttMs: medianRttMs,
    lastRttMs: medianRttMs,
    rttDeltaSumMs: 0,
    rttDeltaCount: 0,
    pingCount: 1,
    lossCount: 0,
    underLoad: phase !== "latency",
    phase,
    continuityId: 1,
  };
}

function indexed(index: LatencyPhaseIndex): LatencyBucket[] {
  return [...index.values()].flat();
}

test("tail appends extend the latency phase index incrementally", () => {
  const index = new LatencyPhaseIndex();
  const history = [bucket(0, "latency", 10)];
  index.update(history, 0);
  history.push(bucket(200, "download", 20));
  index.update(history, 0);

  expect(indexed(index)).toEqual(history);
});

test("a history revision rebuilds replaced and inserted latency buckets", () => {
  const index = new LatencyPhaseIndex();
  const first = bucket(0, "latency", 10);
  const tail = bucket(400, "download", 40);
  const history = [first, tail];
  index.update(history, 0);

  const revised = bucket(0, "latency", 99);
  const inserted = bucket(200, "latency", 20);
  history.splice(0, 2, revised, inserted, tail);
  index.update(history, 1);

  expect([...index.values()]).toEqual([[revised, inserted], [tail]]);
  expect(indexed(index)).not.toContain(first);
});
