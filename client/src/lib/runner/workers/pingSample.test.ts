import { expect, test } from "bun:test";
import { reflectorHandlingMs } from "./pingSample";

test("reflector timing rejects impossible clock pairs without clamping", () => {
  expect(reflectorHandlingMs(0, "0")).toBe(0);
  expect(reflectorHandlingMs(1, "1000000")).toBe(1);
  expect(reflectorHandlingMs(0, "1")).toBeUndefined();
  expect(reflectorHandlingMs(0.5, "500001")).toBeUndefined();
  expect(reflectorHandlingMs(Infinity, "0")).toBeUndefined();
  expect(reflectorHandlingMs(-1, "0")).toBeUndefined();
  expect(reflectorHandlingMs(1e20, "9007199254740992")).toBeUndefined();
});
