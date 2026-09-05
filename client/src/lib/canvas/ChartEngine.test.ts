import { stubGlobals } from "../test-helpers.test";
import { expect, test } from "bun:test";
import {
  ChartEngine,
  type ChartData,
  type ChartPresentation,
} from "./ChartEngine";
import type { LatencyBucket, ThroughputSample } from "../runner/contract";

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

function canvasEnvironment(reducedMotion: boolean) {
  const counts = { paths: 0 };
  const context = new Proxy({} as CanvasRenderingContext2D, {
    get: (_target, property) => {
      if (property === "createLinearGradient")
        return () => ({ addColorStop() {} });
      if (property === "beginPath")
        return () => {
          counts.paths++;
        };
      return () => {};
    },
  });
  const canvas = {
    width: 0,
    height: 0,
    getContext: () => context,
    getBoundingClientRect: () => ({ width: 600, height: 240 }),
  } as unknown as HTMLCanvasElement;
  const restore = stubGlobals({
    window: {
      devicePixelRatio: 1,
      matchMedia: () => ({
        matches: reducedMotion,
        addEventListener() {},
        removeEventListener() {},
      }),
    },
    document: {
      documentElement: {},
      createElement: () => ({ ...canvas }),
      addEventListener() {},
      removeEventListener() {},
    },
    getComputedStyle: () => ({ getPropertyValue: () => "" }),
  });
  return { canvas, counts, restore };
}

test("reduced motion snaps the camera and renders new latency glyphs without animation", () => {
  const { canvas, restore } = canvasEnvironment(true);

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
    current = {
      ...current,
      latencyEnabled: true,
      latency: [
        {
          t: 100,
          startT: 0,
          endT: 200,
          medianRttMs: 20,
          p95RttMs: 20,
          maxRttMs: 20,
          firstRttMs: 20,
          lastRttMs: 20,
          rttDeltaSumMs: 0,
          rttDeltaCount: 0,
          pingCount: 1,
          lossCount: 0,
          underLoad: true,
          phase: "download",
          continuityId: 1,
        },
      ],
    };
    engine.wake();
    expect(engine.render(148)).toBe(false);
    engine.destroy();
  } finally {
    restore();
  }
});

test("long history is cached across camera, hover, and glyph frames", () => {
  const { canvas, counts, restore } = canvasEnvironment(false);

  const throughput: ThroughputSample[] = Array.from(
    { length: 2_000 },
    (_, index) => ({
      t: index * 2,
      bytesPerSec: 100_000 + (index % 5) * 100,
      bytesCumulative: index * 200_000,
      dir: "down",
      phase: "download",
      continuityId: 1,
    }),
  );
  const latency: LatencyBucket[] = Array.from(
    { length: 2_000 },
    (_, index) => ({
      t: index * 2,
      startT: index * 2,
      endT: index * 2 + 2,
      medianRttMs: 20,
      p95RttMs: 22,
      maxRttMs: 24,
      firstRttMs: 20,
      lastRttMs: 20,
      rttDeltaSumMs: 0,
      rttDeltaCount: 0,
      pingCount: 1,
      lossCount: 0,
      underLoad: false,
      phase: "latency",
      continuityId: 1,
    }),
  );
  try {
    let current = data({
      throughput,
      latency,
      latencyEnabled: true,
      timelineT: 4_000,
    });
    const engine = new ChartEngine(() => current);
    engine.attach(canvas);
    engine.render(0);

    const initialPaths = counts.paths;
    // Entering glyph work is bounded to the recent tail, not all 2,000 buckets.
    for (let now = 16; now <= 64; now += 16) engine.render(now);
    expect(counts.paths - initialPaths).toBeLessThan(500);
    engine.render(200);

    const beforeHover = counts.paths;
    for (let index = 0; index < 20; index++) {
      engine.setHover(50 + index * 20);
      engine.render(70 + index);
    }
    expect(counts.paths - beforeHover).toBeLessThan(500);

    // A camera change rebuilds once; subsequent easing frames only compose it.
    current = { ...current, timelineT: 12_000 };
    engine.wake();
    engine.render(100);
    const beforeCameraFrames = counts.paths;
    for (let now = 116; now <= 420; now += 16) engine.render(now);
    expect(counts.paths - beforeCameraFrames).toBeLessThan(1_000);
    engine.destroy();
  } finally {
    restore();
  }
});

test("equal simultaneous result labels retain distinct lane identities", () => {
  let current = data({
    phase: "complete",
    timelineT: 2_000,
    throughput: (["down", "up"] as const).flatMap((dir) =>
      [500, 1_500].map((t) => ({
        t,
        bytesPerSec: 100_000,
        bytesCumulative: t * 100,
        dir,
        phase: "bidirectional" as const,
        continuityId: 0,
      })),
    ),
    resultRates: { bidiDown: 100_000, bidiUp: 100_000 },
  });
  let published!: ChartPresentation;
  const engine = new ChartEngine(
    () => current,
    (next) => (published = next),
  );
  engine.render(100);
  expect(published.phaseStats.map((stat) => stat.lane)).toEqual([
    "bidiDown",
    "bidiUp",
  ]);
  const [down, up] = published.phaseStats;
  expect(down!.x).toBe(up!.x);
  expect(down!.y).toBe(up!.y);
  expect(down!.bytesPerSec).toBe(up!.bytesPerSec);

  current = {
    ...current,
    resultRates: { bidiDown: 200_000, bidiUp: 100_000 },
  };
  engine.wake();
  engine.render(116);
  expect(published.phaseStats.map((stat) => stat.lane)).toEqual([
    "bidiDown",
    "bidiUp",
  ]);
  engine.destroy();
});
