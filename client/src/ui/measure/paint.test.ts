import { afterEach, beforeEach, expect, test } from "bun:test";
import { scope, signal } from "../../lib/signal";
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

/** Runs the display's frames at `hz` for `count` frames and returns the times `draw` ran. */
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

test("a painter whose view is away draws nothing until it returns, then draws once", () => {
  const away = signal(false);
  const drawn: number[] = [];
  let live = true;
  const [wake, dispose] = scope(() => painter((now) => (drawn.push(now), live), away));
  const tick = (now: number) => frames.shift()?.(now);
  wake();
  tick(0);
  away.set(true);
  tick(20);
  wake();
  tick(40);
  expect(drawn).toEqual([0]);
  live = false;
  away.set(false);
  tick(60);
  tick(80);
  expect(drawn).toEqual([0, 60]);
  dispose();
});

test("a queued resize never paints or restarts after its view is disposed", async () => {
  let draws = 0;
  const [wake, dispose] = scope(() => painter(() => (draws++, true)));
  wake(true);
  dispose();
  await Promise.resolve();
  frames.shift()?.(0);
  expect(draws).toBe(0);
  wake();
  frames.shift()?.(20);
  expect(draws).toBe(0);
});

test("painters on a beat draw together on the frame after it moves, and every 250 ms without it", () => {
  let beat = 0;
  const drawn: [string, number][] = [];
  const [wakes, dispose] = scope(() =>
    ["card", "strip"].map((name) =>
      painter(
        (now) => (drawn.push([name, now]), true),
        undefined,
        () => beat,
      ),
    ),
  );
  for (const wake of wakes) wake();
  const tick = (now: number) => {
    const due = frames;
    frames = [];
    for (const frame of due) frame(now);
  };
  tick(1000);
  tick(1017);
  beat++;
  tick(1033);
  tick(1050);
  for (let now = 1067; now <= 1300; now += 16.5) tick(now);
  expect(drawn.map(([, now]) => Math.round(now))).toEqual([1000, 1000, 1033, 1033, 1298, 1298]);
  dispose();
});
