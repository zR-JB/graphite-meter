import { test, expect } from "bun:test";
import { nextTransferBytes, type SizerCfg } from "./autosize";

const CFG: SizerCfg = {
  targetMs: 1000,
  minBytes: 1000,
  maxBytes: 10_000_000,
  alpha: 0.5,
  stepUp: 2,
  stepDown: 0.5,
};

test("elapsed well under target: size grows, clamped by stepUp", () => {
  const { bytes, ewma } = nextTransferBytes(10_000, 200, 0, CFG);
  expect(bytes).toBe(20_000); // 10_000 * stepUp: the wanted 5x is clamped
  expect(ewma).toBe(50_000); // seeded straight from the observed rate
});

test("elapsed well over target: size shrinks, clamped by stepDown", () => {
  const { bytes, ewma } = nextTransferBytes(10_000, 5_000, 0, CFG);
  expect(bytes).toBe(5_000); // 10_000 * stepDown: the wanted 0.2x is clamped
  expect(ewma).toBe(2_000);
});

test("elapsed exactly at target: size is unchanged", () => {
  const { bytes, ewma } = nextTransferBytes(10_000, 1_000, 0, CFG);
  expect(bytes).toBe(10_000);
  expect(ewma).toBe(10_000);
});

test("non-zero prevEwma is smoothed rather than reseeded", () => {
  const { ewma } = nextTransferBytes(10_000, 1_000, 20_000, CFG);
  // observed = 10_000 bytes/sec; alpha 0.5 blend with the running 20_000 EWMA.
  expect(ewma).toBe(0.5 * 10_000 + 0.5 * 20_000);
});

test("result is clamped to minBytes/maxBytes", () => {
  const grownPastMax = nextTransferBytes(9_000_000, 100, 0, CFG);
  expect(grownPastMax.bytes).toBe(CFG.maxBytes);

  const shrunkPastMin = nextTransferBytes(1_500, 10_000, 0, CFG);
  expect(shrunkPastMin.bytes).toBe(CFG.minBytes);
});

test("zero or negative elapsed leaves size/ewma unchanged (avoids divide-by-zero)", () => {
  expect(nextTransferBytes(10_000, 0, 5_000, CFG)).toEqual({
    bytes: 10_000,
    ewma: 5_000,
  });
  expect(nextTransferBytes(10_000, -50, 5_000, CFG)).toEqual({
    bytes: 10_000,
    ewma: 5_000,
  });
});
