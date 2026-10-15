import { expect, test } from "bun:test";
import { keyed } from "./splitter";

test("arrows move the edge their own way in 16 or 48 px steps, within the panel's limits", () => {
  for (const [key, shift, edge, value, max, expected] of [
    ["ArrowRight", false, "right", 420, 720, 436],
    ["ArrowRight", true, "right", 420, 720, 468],
    ["ArrowLeft", false, "right", 420, 720, 404],
    // A panel whose splitter sits on its left edge widens as the edge moves left.
    ["ArrowLeft", false, "left", 420, 720, 436],
    ["ArrowRight", true, "left", 420, 720, 372],
    // At a limit the width stays where it is.
    ["ArrowLeft", false, "left", 400, 400, 400],
    ["ArrowLeft", false, "right", 365, 720, 360],
    ["Home", false, "left", 500, 720, 360],
    ["End", false, "left", 380, 400, 400],
    ["Enter", false, "right", 400, 720, "reset"],
    [" ", false, "left", 400, 720, "reset"],
    ["Tab", false, "left", 400, 720, null],
  ] as const)
    expect(keyed(key, shift, edge, [value, 360, max]), `${key} ${edge} ${value}`).toEqual(expected);
});
