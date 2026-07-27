import { test, expect } from "bun:test";
import { DEFAULT_TUNING, tuned } from "./tuning";

// The knobs exist so a benchmark can vary them, and measuring a default has to
// measure production. The pool is the one default the benchmark moved: 256 MiB
// beat 64 MiB by 10.9%, see docs/BENCHMARKS.md.
test("the defaults are the shipped constants", () => {
  expect(DEFAULT_TUNING).toEqual({
    readBufBytes: 1048576,
    reader: "byob",
    reportGapMs: 50,
    uploadTotalPoolBytes: 268435456,
    targetPostMs: 500,
    minPostBytes: 131072,
    uploadBody: "blob",
    uploadDrain: "arrayBuffer",
    writeChunkBytes: 4194304,
    congestionControl: "throughput",
    datagramClockEvery: 1,
  });
});

test("an absent tune is the defaults, and a partial one overrides only its keys", () => {
  expect(tuned()).toBe(DEFAULT_TUNING);
  expect(tuned({ readBufBytes: 4096 })).toEqual({
    ...DEFAULT_TUNING,
    readBufBytes: 4096,
  });
});
