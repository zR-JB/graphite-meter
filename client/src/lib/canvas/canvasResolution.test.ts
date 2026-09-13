import { expect, test } from "bun:test";
import { canvasPixelRatio } from "./canvasResolution";

test("normal canvas resolution retains the existing 2x cap", () => {
  expect(canvasPixelRatio(1, 1)).toBe(1);
  expect(canvasPixelRatio(2, 1)).toBe(2);
  expect(canvasPixelRatio(3, 1)).toBe(2);
});

test("pinch zoom gains bounded half-step resolution headroom", () => {
  expect(canvasPixelRatio(1, 1.2)).toBe(1.5);
  expect(canvasPixelRatio(2, 1.1)).toBe(2.5);
  expect(canvasPixelRatio(2, 1.5)).toBe(3);
  expect(canvasPixelRatio(2, 2)).toBe(4);
  expect(canvasPixelRatio(2, 4)).toBe(4);
});

test("density changes and returning to a visible tab refresh resolution without a layout resize", async () => {
  const { stubGlobals } = await import("../test-helpers.test");
  const { watchCanvasPixelRatio } = await import("./canvasResolution");
  const media: EventTarget[] = [];
  const viewport = Object.assign(new EventTarget(), { scale: 1 });
  const browser = Object.assign(new EventTarget(), {
    devicePixelRatio: 1,
    visualViewport: viewport,
    matchMedia: () => {
      const query = new EventTarget();
      media.push(query);
      return query;
    },
  });
  const document = Object.assign(new EventTarget(), { hidden: false });
  const restore = stubGlobals({ window: browser, document });
  let changes = 0;
  const stop = watchCanvasPixelRatio(() => changes++);
  try {
    browser.devicePixelRatio = 1.5;
    media[0].dispatchEvent(new Event("change"));
    expect(changes).toBe(1);
    expect(media).toHaveLength(2);
    browser.devicePixelRatio = 2;
    document.dispatchEvent(new Event("visibilitychange"));
    expect(changes).toBe(2);
    viewport.scale = 1.5;
    viewport.dispatchEvent(new Event("resize"));
    expect(changes).toBe(3);
    stop();
    viewport.scale = 2;
    viewport.dispatchEvent(new Event("resize"));
    expect(changes).toBe(3);
  } finally {
    stop();
    restore();
  }
});
