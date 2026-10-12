import { afterEach, beforeEach, expect, test } from "bun:test";
import { scope } from "../../lib/signal";
import { painter } from "./paint";

let frames: ((now: number) => void)[] = [];
const saved = globalThis.requestAnimationFrame;
beforeEach(() => {
  frames = [];
  globalThis.requestAnimationFrame = (callback) => frames.push(callback);
});
afterEach(() => {
  globalThis.requestAnimationFrame = saved;
});

/** Runs the display's frames at `hz` for `count` frames; the times `draw` ran. */
function display(hz: number, count: number): number[] {
  const drawn: number[] = [];
  const [, dispose] = scope(() => painter((now) => (drawn.push(Math.round(now)), true))());
  for (let frame = 0; frame < count; frame++) frames.shift()?.((frame * 1000) / hz);
  dispose();
  return drawn;
}

test("a live painter draws every frame at 60 Hz and every other frame at 120 Hz", () => {
  expect(display(60, 4)).toEqual([0, 17, 33, 50]);
  expect(display(120, 6)).toEqual([0, 17, 33]);
});
