import { afterAll, expect, test } from "bun:test";

/** A page whose frames and visibility the test drives. */
const frames = new Map<number, FrameRequestCallback>();
let nextFrame = 0;
const listeners = new Map<string, () => void>();
const page = {
  hidden: false,
  addEventListener: (type: string, listener: () => void) => listeners.set(type, listener),
};
const saved = {
  document: globalThis.document,
  raf: globalThis.requestAnimationFrame,
  caf: globalThis.cancelAnimationFrame,
};
Object.assign(globalThis, {
  document: page,
  requestAnimationFrame: (callback: FrameRequestCallback) => (
    frames.set(++nextFrame, callback), nextFrame
  ),
  cancelAnimationFrame: (id: number) => frames.delete(id),
});
afterAll(() =>
  Object.assign(globalThis, {
    document: saved.document,
    requestAnimationFrame: saved.raf,
    cancelAnimationFrame: saved.caf,
  }),
);
const paint = (now: number) => {
  const due = [...frames.values()];
  frames.clear();
  for (const callback of due) callback(now);
};
const show = (visible: boolean) => {
  page.hidden = !visible;
  listeners.get("visibilitychange")?.();
};
// A module instance of its own, which sees only this page.
const fresh = "./frame?visibility";
const { animate }: typeof import("./frame") = await import(fresh);

test("a hidden page asks for no frames, and painting resumes where it shows again", () => {
  const painted: number[] = [];
  const stop = animate((now) => (painted.push(now), true));
  const once: number[] = [];
  animate((now) => (once.push(now), false));
  paint(16);
  paint(32);
  expect([painted, once]).toEqual([[16, 32], [16]]);
  show(false);
  expect(frames.size).toBe(0);
  show(true);
  paint(5_000);
  expect(painted).toEqual([16, 32, 5_000]);
  stop();
  expect(frames.size).toBe(0);
  show(false);
  show(true);
  expect(frames.size).toBe(0);
});
