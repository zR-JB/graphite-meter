import { expect, test } from "bun:test";
import {
  ChartEngine,
  type ChartData,
  type ChartPresentation,
} from "./ChartEngine";

function data(overrides: Partial<ChartData> = {}): ChartData {
  return {
    throughput: [],
    latency: [],
    latencyRevision: 0,
    latencyEnabled: false,
    phase: "latency",
    phaseStartedAtMs: 0,
    timelineT: 0,
    runSeq: 1,
    scaleBytesPerSec: 125_000,
    latencyScaleMs: 50,
    resultRates: {},
    ...overrides,
  };
}

test("camera keeps a run-wide origin and eases a large live time advance", () => {
  let current = data();
  let published!: ChartPresentation;
  const engine = new ChartEngine(
    () => current,
    (next) => (published = next),
  );

  expect(engine.render(100)).toBe(false);
  expect(published.layout.viewport.tMin).toBe(0);

  current = { ...current, phase: "download", timelineT: 5_000 };
  engine.wake();
  expect(engine.render(116)).toBe(true);
  expect(published.layout.viewport.tMin).toBe(0);
  expect(published.layout.viewport.tMax).toBeGreaterThan(4_000);
  expect(published.layout.viewport.tMax).toBeLessThan(7_000);

  current = { ...current, phase: "upload", timelineT: 8_000 };
  engine.wake();
  expect(engine.render(132)).toBe(true);
  expect(published.layout.viewport.tMin).toBe(0);

  let now = 148;
  let active = true;
  for (let i = 0; i < 200 && active; i++) {
    active = engine.render(now);
    now += 16;
  }
  expect(active).toBe(false);
  expect(published.layout.viewport.tMax).toBe(10_000);

  const throughput = current.throughput;
  const latency = current.latency;
  current = {
    ...current,
    phase: "complete",
    timelineT: 8_000,
    throughput,
    latency,
  };
  engine.wake();
  active = engine.render(now);
  for (let i = 0; i < 400 && active; i++) {
    now += 16;
    active = engine.render(now);
  }
  expect(active).toBe(false);
  expect(published.layout.viewport.tMin).toBe(0);
  expect(throughput).toEqual([]);
  expect(latency).toEqual([]);
  engine.destroy();
});

test("reduced motion snaps the camera to its target and parks", () => {
  const globals = globalThis as unknown as Record<string, unknown>;
  const previousWindow = globals.window;
  const previousDocument = globals.document;
  const previousGetComputedStyle = globals.getComputedStyle;
  const context = new Proxy({} as CanvasRenderingContext2D, {
    get: (_target, property) =>
      property === "createLinearGradient"
        ? () => ({ addColorStop: () => {} })
        : () => {},
  });
  const canvas = {
    width: 0,
    height: 0,
    getContext: () => context,
    getBoundingClientRect: () => ({ width: 600, height: 240 }),
  } as unknown as HTMLCanvasElement;
  const media = {
    matches: true,
    addEventListener: () => {},
    removeEventListener: () => {},
  } as unknown as MediaQueryList;
  const documentValue = {
    documentElement: {},
    createElement: () => ({ ...canvas }),
  } as unknown as Document;

  Object.assign(globalThis, {
    window: { devicePixelRatio: 1, matchMedia: () => media },
    document: documentValue,
    getComputedStyle: () => ({ getPropertyValue: () => "" }),
  });

  try {
    let current = data();
    let published!: ChartPresentation;
    const engine = new ChartEngine(
      () => current,
      (next) => (published = next),
    );
    engine.attach(canvas);
    expect(engine.render(100)).toBe(false);
    current = { ...current, phase: "download", timelineT: 5_000 };
    engine.wake();
    expect(engine.render(116)).toBe(true);
    expect(published.layout.viewport.tMax).toBe(7_000);
    expect(engine.render(132)).toBe(false);
    engine.destroy();
  } finally {
    Object.defineProperty(globalThis, "window", {
      value: previousWindow,
      configurable: true,
      writable: true,
    });
    Object.defineProperty(globalThis, "document", {
      value: previousDocument,
      configurable: true,
      writable: true,
    });
    Object.defineProperty(globalThis, "getComputedStyle", {
      value: previousGetComputedStyle,
      configurable: true,
      writable: true,
    });
  }
});
