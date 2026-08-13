import { expect, test } from "bun:test";
import { pingSample, pingSampleContextTime } from "./pingSample";

test("ping outcome time translates across different performance origins", () => {
  const sample = pingSample(12, false, 350, 10_000);

  expect(sample).toEqual({
    rtt: 12,
    lost: false,
    observedAtEpochMs: 10_350,
  });
  expect(pingSampleContextTime(sample, 9_500)).toBe(850);
});
