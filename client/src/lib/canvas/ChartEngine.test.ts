import "../state/runes.testutil";
import { stubGlobals } from "../test-helpers.testutil";
import { expect, test } from "bun:test";
import type { ChartData, ChartPresentation } from "./ChartEngine";
import type { LatencyBucket, ThroughputSample } from "../runner/contract";
import { appendThroughputSample } from "../runner/series";

const { ChartEngine } = await import("./ChartEngine");

function data(overrides: Partial<ChartData> = {}): ChartData {
  return {
    throughput: [],
    throughputRevision: 0,
    latency: [],
    latencyRevision: 0,
    latencyEnabled: false,
    phase: "latency",
    phaseStartedAtMs: 0,
    timelineAt: () => 0,
    runSeq: 1,
    scaleBytesPerSec: 125_000,
    latencyScaleMs: 50,
    resultRates: {},
    ...overrides,
  };
}

test("the live axis follows the run timeline every frame; a finished run glides to its span", () => {
  let current = data({ phase: "download", timelineAt: (now) => now });
  let published!: ChartPresentation;
  let timeScale = 0;
  const engine = new ChartEngine(
    current,
    (next) => (published = next),
    (tMax) => (timeScale = tMax),
  );
  expect(engine.render(1_000)).toBe(true);
  expect(published.layout.viewport).toMatchObject({ tMin: 0, tMax: 4_000 });
  engine.render(5_000);
  expect(timeScale).toBe(7_000);
  engine.render(5_016);
  expect(timeScale).toBe(7_016);

  const throughput = [0, 5_000].map((t) => ({
    t,
    bytesPerSec: 100_000,
    bytesCumulative: t,
    dir: "down" as const,
    phase: "download" as const,
    continuityId: 1,
  }));
  current = { ...current, phase: "complete", throughput };
  engine.update(current);
  let now = 5_032;
  let active = engine.render(now);
  expect(timeScale).toBe(7_016);
  const glide: number[] = [];
  for (let i = 0; i < 100 && active; i++) {
    now += 16;
    active = engine.render(now);
    glide.push(timeScale);
  }
  expect(active).toBe(false);
  expect(glide.every((t, i) => i === 0 || t <= glide[i - 1])).toBe(true);
  expect(timeScale).toBe(5_100);
  expect(published.layout.viewport.tMax).toBe(5_100);
  engine.destroy();
});

function canvasEnvironment() {
  const counts = { paths: 0, curves: [] as number[][] };
  const context = new Proxy({} as CanvasRenderingContext2D, {
    get: (_target, property) => {
      if (property === "createLinearGradient")
        return () => ({ addColorStop() {} });
      if (property === "beginPath")
        return () => {
          counts.paths++;
        };
      if (property === "bezierCurveTo")
        return (...points: number[]) => counts.curves.push(points);
      return () => {};
    },
  });
  const canvas = {
    width: 0,
    height: 0,
    clientWidth: 600,
    clientHeight: 240,
    getContext: () => context,
    addEventListener() {},
    removeEventListener() {},
    getBoundingClientRect: () => ({ width: 600, height: 240 }),
  } as unknown as HTMLCanvasElement;
  const restore = stubGlobals({
    window: {
      devicePixelRatio: 1,
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

test("saved duplicate terminal points render and hover at the last value without a vertical segment", () => {
  const { canvas, counts, restore } = canvasEnvironment();
  const throughput: ThroughputSample[] = [
    [0, 1000],
    [500, 2000],
    [500, 500],
  ].map(([t, bytesPerSec]) => ({
    t,
    bytesPerSec,
    bytesCumulative: t,
    dir: "down",
    phase: "download",
    continuityId: 1,
  }));
  let published!: ChartPresentation;
  const engine = new ChartEngine(
    data({ throughput, phase: "complete", timelineAt: () => 500 }),
    (next) => (published = next),
  );
  try {
    engine.attach(canvas);
    engine.render(0);
    expect(engine.inspect(published.layout.x(500))?.bytesPerSec).toBeCloseTo(
      500,
      8,
    );
    expect(counts.curves).toHaveLength(2); // Filled area and line each have one interval.
    for (const [x1, _y1, _x2, _y2, x, y] of counts.curves) {
      expect(x1).toBeLessThan(x);
      expect(x).toBeCloseTo(published.layout.x(500), 8);
      expect(y).toBeCloseTo(published.layout.throughputY(500), 8);
    }
    expect(throughput).toHaveLength(3); // Loading a saved record does not rewrite its evidence.
  } finally {
    engine.destroy();
    restore();
  }
});

test("interleaved equal-time replacement invalidates the lane cache even when another lane subsequently appends", () => {
  const { canvas, restore } = canvasEnvironment();
  const sample = (
    t: number,
    dir: "down" | "up",
    bytesPerSec: number,
  ): ThroughputSample => ({
    t,
    dir,
    bytesPerSec,
    bytesCumulative: t,
    phase: "bidirectional",
    continuityId: 1,
  });
  const current = data({
    phase: "bidirectional",
    throughput: [
      sample(0, "down", 1000),
      sample(0, "up", 2000),
      sample(1000, "down", 1000),
      sample(1000, "up", 2000),
    ],
  });
  let published!: ChartPresentation;
  const engine = new ChartEngine(current, (next) => (published = next));
  try {
    engine.attach(canvas);
    engine.render(0);
    expect(
      engine.inspect(published.layout.x(1000))?.downBytesPerSec,
    ).toBeCloseTo(1000, 8);
    expect(
      appendThroughputSample(current.throughput, sample(1000, "down", 500)),
    ).toBe(true);
    current.throughputRevision++;
    appendThroughputSample(current.throughput, sample(2000, "up", 700));
    engine.update(current);
    engine.render(16);
    expect(engine.inspect(published.layout.x(1000))).toMatchObject({
      downBytesPerSec: 500,
      upBytesPerSec: 2000,
    });
    expect(engine.inspect(published.layout.x(2000))?.upBytesPerSec).toBeCloseTo(
      700,
      8,
    );
  } finally {
    engine.destroy();
    restore();
  }
});

test("long history is cached across camera, hover, and glyph frames", () => {
  const { canvas, counts, restore } = canvasEnvironment();

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
      pingCount: 1,
      timeoutCount: 0,
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
      timelineAt: () => 4_000,
    });
    const engine = new ChartEngine(current);
    engine.attach(canvas);
    engine.render(0);

    const initialPaths = counts.paths;
    // Entering glyph work is bounded to the recent tail, not all 2,000 buckets.
    for (let now = 16; now <= 64; now += 16) engine.render(now);
    expect(counts.paths - initialPaths).toBeLessThan(500);
    engine.render(200);

    const beforeHover = counts.paths;
    for (let index = 0; index < 20; index++) {
      expect(engine.inspect(50 + index * 20)).not.toBeNull();
    }
    expect(counts.paths).toBe(beforeHover);

    const beforeClock = counts.paths;
    engine.update({ ...current, timelineAt: (now) => 4_000 + now * 20 });
    for (let now = 216; now <= 520; now += 16) engine.render(now);
    const clockPaths = counts.paths - beforeClock;
    const beforeData = counts.paths;
    engine.update({ ...current, latencyRevision: 1 });
    engine.render(540);
    expect(clockPaths).toBeLessThan((counts.paths - beforeData) / 10);
    engine.destroy();
  } finally {
    restore();
  }
});

test("equal simultaneous result labels retain distinct lane identities", () => {
  let current = data({
    phase: "complete",
    timelineAt: () => 2_000,
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
  const engine = new ChartEngine(current, (next) => (published = next));
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
  engine.update(current);
  engine.render(116);
  expect(published.phaseStats.map((stat) => stat.lane)).toEqual([
    "bidiDown",
    "bidiUp",
  ]);
  engine.destroy();
});

test("inspection retains a time position through gaps without inventing latency", () => {
  const { canvas, counts, restore } = canvasEnvironment();
  try {
    let current = data({ latencyEnabled: true });
    let published!: ChartPresentation;
    const engine = new ChartEngine(current, (next) => (published = next));
    engine.attach(canvas);
    engine.render(0);
    expect(engine.inspect(100)).toBeNull();
    current = {
      ...current,
      latency: [
        {
          t: 400,
          startT: 350,
          endT: 450,
          phase: "latency",
          continuityId: 1,
          underLoad: false,
          medianRttMs: null,
          p95RttMs: null,
          maxRttMs: null,
          pingCount: 1,
          timeoutCount: 1,
        },
      ],
    };
    engine.update(current);
    engine.render(16);
    const before = counts.paths;
    expect(engine.inspect(published.layout.x(400))).toMatchObject({
      rtt: null,
      timeoutCount: 1,
      pingCount: 1,
    });
    expect(engine.inspect(published.layout.x(2_000))).toMatchObject({
      t: 2_000,
      rtt: null,
      timeoutCount: 0,
      pingCount: 0,
    });
    expect(counts.paths).toBe(before);
    engine.destroy();
  } finally {
    restore();
  }
});

test("canvas sizing ignores entry transforms and recovers after a layout resize", () => {
  const { canvas, restore } = canvasEnvironment();
  let published!: ChartPresentation;
  const engine = new ChartEngine(data(), (next) => (published = next));
  canvas.getBoundingClientRect = () =>
    ({ width: 591, height: 236.4 }) as DOMRect;
  try {
    engine.attach(canvas);
    engine.render(0);
    expect(canvas.width).toBe(600);
    expect(canvas.height).toBe(240);
    expect(published.layout.width).toBe(600);
    Object.defineProperty(canvas, "clientWidth", { value: 450 });
    engine.invalidateTheme();
    engine.render(100);
    expect(canvas.width).toBe(450);
    expect(published.layout.width).toBe(450);
  } finally {
    engine.destroy();
    restore();
  }
});
