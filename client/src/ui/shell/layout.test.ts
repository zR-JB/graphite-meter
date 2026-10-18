import { expect, test } from "bun:test";
import { resolveDocks } from "./layout";

const both = { left: true, right: true };

test("docks keep their preference within 360 to 720 px while the stage keeps 800 px", () => {
  expect(resolveDocks(1_200, { left: 420, right: 0 }, { left: true, right: false }).left).toEqual({
    column: 400,
    max: 400,
  });
  expect(resolveDocks(1_520, { left: 420, right: 420 }, both)).toEqual({
    left: { column: 360, max: 360 },
    right: { column: 360, max: 360 },
  });
  expect(resolveDocks(2_400, { left: 100, right: 900 }, both)).toEqual({
    left: { column: 360, max: 720 },
    right: { column: 720, max: 720 },
  });
  expect(
    resolveDocks(1_000, { left: 420, right: 420 }, { left: false, right: false }).left.column,
  ).toBe(0);
});

test("past the budget both shrink in proportion, unless one was resized last and keeps its width", () => {
  const shared = resolveDocks(1_700, { left: 600, right: 420 }, both);
  expect([shared.left.column, shared.right.column]).toEqual([
    360 + Math.floor((180 * 240) / 300),
    900 - 504,
  ]);
  const dragged = resolveDocks(1_700, { left: 600, right: 420 }, both, "left");
  expect([dragged.left.column, dragged.right.column]).toEqual([540, 360]);
  // Widening the window brings the other side back to its own preference, which the drag never rewrote.
  const wide = resolveDocks(2_000, { left: 600, right: 420 }, both, "left");
  expect([wide.left.column, wide.right.column]).toEqual([600, 420]);
});
