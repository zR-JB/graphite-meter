import { expect, test } from "bun:test";
import type { ThroughputResult } from "../runner/contract";
import { bidirectionalResultPresentation } from "./bidirectionalResult";

function lane(reportedBytesPerSec: number): ThroughputResult {
  return {
    reportedBytesPerSec,
  } as ThroughputResult;
}

test("a down-only bidirectional partial has no combined headline", () => {
  const model = bidirectionalResultPresentation({ down: lane(700), up: null });

  expect(model.combinedBytesPerSec).toBeNull();
  expect(model.survivingDirection).toBe("down");
  expect(model.down?.reportedBytesPerSec).toBe(700);
});

test("an up-only bidirectional partial has no combined headline", () => {
  const model = bidirectionalResultPresentation({ down: null, up: lane(300) });

  expect(model.combinedBytesPerSec).toBeNull();
  expect(model.survivingDirection).toBe("up");
  expect(model.up?.reportedBytesPerSec).toBe(300);
});

test("two bidirectional lanes produce the only combined headline", () => {
  const model = bidirectionalResultPresentation({
    down: lane(700),
    up: lane(300),
  });

  expect(model.combinedBytesPerSec).toBe(1_000);
  expect(model.survivingDirection).toBeNull();
});
