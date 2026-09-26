import { test, expect } from "bun:test";
import { sweepTarget } from "./gaugeSweep";

test("a transfer gauge stays neutral until authoritative evidence arrives", () => {
  const download = {
    phase: "download",
    valueBytesPerSec: 0,
    scaleBytesPerSec: 1000,
    latencyScaleMs: 100,
    rtt: 0,
    completedKind: "speed",
  } as const;
  expect(sweepTarget({ ...download, throughputEvidence: false })).toBe(0.5);
  expect(sweepTarget({ ...download, throughputEvidence: true })).toBe(0);
});
