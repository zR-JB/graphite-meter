import { expect, test } from "bun:test";
import { bidirectionalResultPresentation } from "./bidirectionalResult";

test("only two receiver-reported lanes produce a combined headline", () => {
  expect(bidirectionalResultPresentation(700, 300)).toEqual({
    down: 700,
    up: 300,
    combinedBytesPerSec: 1000,
    survivingDirection: null,
  });
  expect(bidirectionalResultPresentation(700, null)).toEqual({
    down: 700,
    up: null,
    combinedBytesPerSec: null,
    survivingDirection: "down",
  });
  expect(bidirectionalResultPresentation(undefined, 300)).toEqual({
    down: null,
    up: 300,
    combinedBytesPerSec: null,
    survivingDirection: "up",
  });
});

test("zero remains measured evidence while missing lanes remain missing", () => {
  expect(bidirectionalResultPresentation(0, 0).combinedBytesPerSec).toBe(0);
  expect(bidirectionalResultPresentation(0, null).survivingDirection).toBe(
    "down",
  );
  expect(bidirectionalResultPresentation(undefined, 0).survivingDirection).toBe(
    "up",
  );
  expect(bidirectionalResultPresentation(null, undefined)).toEqual({
    down: null,
    up: null,
    combinedBytesPerSec: null,
    survivingDirection: null,
  });
});
