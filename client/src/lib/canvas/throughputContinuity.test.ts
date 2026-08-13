import { expect, test } from "bun:test";
import type { ThroughputSample } from "../runner/contract";
import { interpolateConnectedAt } from "./hoverInterp";
import { throughputSamplesContinuous } from "./throughputContinuity";

function sample(
  t: number,
  continuityId: number,
  bytesPerSec = 1_000,
): ThroughputSample {
  return {
    t,
    bytesPerSec,
    bytesCumulative: t,
    dir: "up",
    phase: "upload",
    continuityId,
  };
}

test("an irregular authoritative throughput gap remains continuous", () => {
  const samples = [sample(100, 4, 1_000), sample(800, 4, 2_000)];

  expect(throughputSamplesContinuous(samples[0], samples[1])).toBe(true);
  expect(
    interpolateConnectedAt(
      samples,
      450,
      (entry) => entry.bytesPerSec,
      throughputSamplesContinuous,
    ),
  ).toBe(1_500);
});

test("an explicit continuity break remains a throughput and hover break", () => {
  const samples = [sample(100, 4, 1_000), sample(800, 5, 2_000)];

  expect(throughputSamplesContinuous(samples[0], samples[1])).toBe(false);
  expect(
    interpolateConnectedAt(
      samples,
      450,
      (entry) => entry.bytesPerSec,
      throughputSamplesContinuous,
    ),
  ).toBeNull();
});
