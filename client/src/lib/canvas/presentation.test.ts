import { expect, test } from "bun:test";
import {
  PresentationScheduler,
  type PresentationEnvironment,
} from "./presentation";

class FakeEnvironment implements PresentationEnvironment {
  hiddenState = false;
  currentTime = 0;
  #next = 1;
  #frames = new Map<number, FrameRequestCallback>();
  #timers = new Map<number, { at: number; callback: () => void }>();
  #visibility = new Set<() => void>();
  #intersections = new Map<Element, (visible: boolean) => void>();

  hidden = () => this.hiddenState;
  now = () => this.currentTime;
  requestFrame = (callback: FrameRequestCallback) => {
    const id = this.#next++;
    this.#frames.set(id, callback);
    return id;
  };
  cancelFrame = (id: number) => {
    this.#frames.delete(id);
  };
  setTimer = (callback: () => void, delayMs: number) => {
    const id = this.#next++;
    this.#timers.set(id, { at: this.currentTime + delayMs, callback });
    return id;
  };
  clearTimer = (id: number) => {
    this.#timers.delete(id);
  };
  observe = (element: Element, visible: (value: boolean) => void) => {
    this.#intersections.set(element, visible);
    return () => this.#intersections.delete(element);
  };
  onVisibilityChange = (callback: () => void) => {
    this.#visibility.add(callback);
    return () => this.#visibility.delete(callback);
  };

  get pending() {
    return this.#frames.size + this.#timers.size;
  }

  frame(ms = 1000 / 60) {
    this.currentTime += ms;
    const timers = [...this.#timers.entries()].filter(
      ([, timer]) => timer.at <= this.currentTime,
    );
    for (const [id, timer] of timers) {
      this.#timers.delete(id);
      timer.callback();
    }
    const callbacks = [...this.#frames.values()];
    this.#frames.clear();
    for (const callback of callbacks) callback(this.currentTime);
  }

  setHidden(hidden: boolean) {
    this.hiddenState = hidden;
    for (const callback of this.#visibility) callback();
  }

  setVisible(element: Element, visible: boolean) {
    this.#intersections.get(element)?.(visible);
  }
}

test("settled work parks and pointer invalidations coalesce", () => {
  const environment = new FakeEnvironment();
  const scheduler = new PresentationScheduler(environment);
  const element = {} as Element;
  let renders = 0;
  let pointer = 0;
  let renderedPointer = 0;
  const task = scheduler.register(element, () => {
    renders++;
    renderedPointer = pointer;
    return false;
  });

  environment.frame();
  expect(renders).toBe(1);
  expect(environment.pending).toBe(0);
  for (const value of [1, 2, 3]) {
    pointer = value;
    task.invalidate();
  }
  expect(environment.pending).toBe(1);
  environment.frame(40);
  expect(renders).toBe(2);
  expect(renderedPointer).toBe(3);
  expect(environment.pending).toBe(0);
});

test("hidden and offscreen tasks redraw once when visible", () => {
  const environment = new FakeEnvironment();
  const scheduler = new PresentationScheduler(environment);
  const element = {} as Element;
  let renders = 0;
  const task = scheduler.register(element, () => {
    renders++;
    return false;
  });
  environment.frame();

  environment.setHidden(true);
  task.invalidate();
  expect(environment.pending).toBe(0);
  environment.setHidden(false);
  expect(environment.pending).toBe(1);
  environment.frame(40);
  expect(renders).toBe(2);
  expect(environment.pending).toBe(0);

  environment.setVisible(element, false);
  task.invalidate();
  expect(environment.pending).toBe(0);
  environment.setVisible(element, true);
  expect(environment.pending).toBe(1);
  environment.frame(40);
  expect(renders).toBe(3);
  expect(environment.pending).toBe(0);
});

test("unsettled work is capped at 30 presentation frames per second", () => {
  const environment = new FakeEnvironment();
  const scheduler = new PresentationScheduler(environment);
  let renders = 0;
  scheduler.register({} as Element, () => {
    renders++;
    return true;
  });

  for (let i = 0; i < 60; i++) environment.frame();
  expect(renders).toBeLessThanOrEqual(30);
  expect(renders).toBeGreaterThanOrEqual(28);
});

test("a moving hero gauge uses native frames without waking settled charts", () => {
  const environment = new FakeEnvironment();
  const scheduler = new PresentationScheduler(environment);
  let gaugeRenders = 0;
  let chartRenders = 0;
  scheduler.register(
    {} as Element,
    () => {
      gaugeRenders++;
      return gaugeRenders < 6;
    },
    { nativeAnimation: true },
  );
  scheduler.register({} as Element, () => {
    chartRenders++;
    return false;
  });

  for (let i = 0; i < 6; i++) environment.frame();

  expect(gaugeRenders).toBe(6);
  expect(chartRenders).toBe(1);
  expect(environment.pending).toBe(0);
});

test("a hidden native animation parks until its gauge becomes visible", () => {
  const environment = new FakeEnvironment();
  const scheduler = new PresentationScheduler(environment);
  const gauge = {} as Element;
  let renders = 0;
  scheduler.register(
    gauge,
    () => {
      renders++;
      return true;
    },
    { nativeAnimation: true },
  );

  environment.frame();
  environment.setVisible(gauge, false);
  expect(environment.pending).toBe(0);
  environment.frame(100);
  expect(renders).toBe(1);

  environment.setVisible(gauge, true);
  environment.frame();
  expect(renders).toBe(2);
});
