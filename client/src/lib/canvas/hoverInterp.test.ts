import { test, expect } from "bun:test";
import { hasHoverMeasurements } from "./hoverInterp";

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
