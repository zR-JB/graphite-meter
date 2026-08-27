import { test, expect } from "bun:test";
import { hasHoverMeasurements, interpolateConnectedAt } from "./hoverInterp";

test("interpolateConnectedAt refuses to invent a value across a break", () => {
  const samples = [
    { t: 0, v: 10, segment: 1 },
    { t: 10, v: 20, segment: 2 },
  ];
  expect(
    interpolateConnectedAt(
      samples,
      5,
      (sample) => sample.v,
      (left, right) => left.segment === right.segment,
    ),
  ).toBeNull();
});

test("loss-only latency buckets remain hoverable", () => {
  expect(
    hasHoverMeasurements({
      bytesPerSec: null,
      downBytesPerSec: null,
      upBytesPerSec: null,
      rtt: null,
      pingCount: 4,
    }),
  ).toBe(true);

  expect(
    hasHoverMeasurements({
      bytesPerSec: null,
      downBytesPerSec: null,
      upBytesPerSec: null,
      rtt: null,
      pingCount: 0,
    }),
  ).toBe(false);
});
