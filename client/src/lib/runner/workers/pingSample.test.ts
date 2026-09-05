import { expect, test } from "bun:test";
import {
  pingSample,
  pingSampleContextTime,
  reflectorHandlingMs,
} from "./pingSample";

test("ping outcome time translates across different performance origins", () => {
  const sample = pingSample(12, false, 350, 10_000);

  expect(sample).toEqual({
    rtt: 12,
    lost: false,
    observedAtEpochMs: 10_350,
  });
  expect(pingSampleContextTime(sample, 9_500)).toBe(850);
});

test("reflector timing rejects impossible clock pairs without clamping", () => {
  expect(reflectorHandlingMs(0, "0")).toBe(0);
  expect(reflectorHandlingMs(1, "1000000")).toBe(1);
  expect(reflectorHandlingMs(0, "1")).toBeUndefined();
  expect(reflectorHandlingMs(0.5, "500001")).toBeUndefined();
  expect(reflectorHandlingMs(1, undefined)).toBeUndefined();
  expect(reflectorHandlingMs(Infinity, "0")).toBeUndefined();
  expect(reflectorHandlingMs(-1, "0")).toBeUndefined();
  expect(reflectorHandlingMs(1e20, "9007199254740992")).toBeUndefined();
});
