import { expect, test } from "bun:test";
import { replies, stageGraphGeometry, drawStageGraph } from "./stageGraph";
import { monotoneCurve } from "./smoothPath";
import { singleLatencyBucket } from "../runner/series";

test("a stage's replies are its measured buckets; a bucket of timeouts has no point", () => {
  const history = [
    singleLatencyBucket(100, 12, false, "latency"),
    singleLatencyBucket(300, 0, true, "latency"),
    singleLatencyBucket(500, 40, false, "download"),
    singleLatencyBucket(700, 14, false, "latency"),
  ];
  expect(replies(history, "latency")).toEqual([
    { t: 100, t0: 100, t1: 100, ms: 12, lo: 12, hi: 12 },
    { t: 700, t0: 700, t1: 700, ms: 14, lo: 14, hi: 14 },
  ]);
});

const base = {
  latency: [],
  start: 1000,
  span: 4000,
  ceiling: 100,
  baseline: 10,
  latencyTop: 50,
  width: 400,
  plotHeight: 100,
  trackHeight: 20,
};

test("cached moving tails preserve the complete curve and reuse measured geometry", () => {
  const geometry = stageGraphGeometry({
    ...base,
    lanes: [
      Array.from({ length: 80 }, (_, i) => ({
        t: 1000 + i * 50,
        v: 10 + ((i * 37) % 90),
      })),
      Array.from({ length: 40 }, (_, i) => ({
        t: 1000 + i * 100,
        v: 90 - ((i * 13) % 80),
      })),
    ],
  });
  for (let t = 1000; t <= 5000; t += 37) {
    const values = [20 + (t % 79), 80 - (t % 73)];
    const graph = drawStageGraph(geometry, { t, values });
    expect(graph.dots).toBe(geometry.dots);
    expect(graph.bins).toBe(geometry.bins);
    for (let lane = 0; lane < 2; lane++) {
      const tip = { x: geometry.x(t), y: geometry.y(values[lane]) };
      const points = [
        ...geometry.points[lane].filter((point) => point.x < tip.x),
        tip,
      ];
      const line =
        points.length < 2
          ? ""
          : `M${points[0].x} ${points[0].y}` +
            monotoneCurve(points)
              .map(
                ({ control1: a, control2: b, end: e }) =>
                  `C${a.x} ${a.y} ${b.x} ${b.y} ${e.x} ${e.y}`,
              )
              .join("");
      expect(graph.lines[lane]).toBe(line);
      if (!lane)
        expect(graph.area).toBe(
          line
            ? `${line}L${tip.x} ${geometry.plotHeight}L${points[0].x} ${geometry.plotHeight}Z`
            : "",
        );
    }
  }
});

test("a running stage spans its plan and its leading edge carries the glided rate", () => {
  const lane = [0, 1, 2].map((i) => ({ t: 1000 + i * 500, v: 50 }));
  const graph = drawStageGraph(stageGraphGeometry({ ...base, lanes: [lane] }), {
    t: 3000,
    values: [100],
  });
  expect(graph.heads).toEqual([{ x: 200, y: 3 }]);
  expect(graph.bins[0].at(-1)!.t).toBeLessThan(3000);
  expect(graph.area.endsWith("Z")).toBe(true);
});

test("a live lane ends at its head even when the newest bin is centred past it", () => {
  const graph = drawStageGraph(
    stageGraphGeometry({
      ...base,
      lanes: [[1000, 2000, 2961].map((t) => ({ t, v: 50 }))],
    }),
    { t: 2970, values: [80] },
  );
  const [{ x, y }] = graph.heads;
  expect(graph.bins[0].at(-1)!.t).toBeGreaterThan(2970);
  expect(graph.lines[0].endsWith(`${x} ${y}`)).toBe(true);
});

test("a reply below the idle median sits below its line; the track clamps at its top", () => {
  const graph = stageGraphGeometry({
    ...base,
    lanes: [[]],
    latency: [
      { t: 2000, t0: 2000, t1: 2000, ms: 5, lo: 5, hi: 5 },
      { t: 2400, t0: 2400, t1: 2400, ms: 30, lo: 30, hi: 30 },
      { t: 2800, t0: 2800, t1: 2800, ms: 500, lo: 500, hi: 500 },
      { t: 9000, t0: 9000, t1: 9000, ms: 30, lo: 30, hi: 30 },
    ],
  });
  const [below, above, capped] = graph.dots.map((dot) => dot.y);
  expect(graph.dots).toHaveLength(3);
  expect(below).toBeGreaterThan(graph.baselineY!);
  expect(above).toBeLessThan(graph.baselineY!);
  expect(capped).toBe(1.5);
  expect(graph.lines).toEqual([""]);
});

const buckets = (count: number, ms: number, lo = 10, spread = 4) =>
  Array.from({ length: count }, (_, k) => ({
    t: 1000 + (k + 0.5) * ms,
    t0: 1000 + k * ms,
    t1: 1000 + (k + 1) * ms,
    ms: lo + k + spread / 2,
    lo: lo + k,
    hi: lo + k + spread,
  }));

test("each reply bucket is one bar, from its own fastest to its slowest reply", () => {
  const latency = buckets(10, 400);
  const graph = stageGraphGeometry({ ...base, lanes: [[]], latency });
  expect(graph.dots.map(({ t0, t1, lo, hi }) => ({ t0, t1, lo, hi }))).toEqual(
    latency.map(({ t0, t1, lo, hi }) => ({ t0, t1, lo, hi })),
  );
  expect(graph.dots.map((dot) => dot.x)).toEqual(
    latency.map((point) => ((point.t - 1000) / 4000) * 400),
  );
});

test("buckets narrower than a bar's pitch join whole, and a closing bucket only grows the newest bar", () => {
  // 40 s over 400 px: a 4 px pitch holds two 200 ms buckets.
  const wide = { ...base, span: 40_000, lanes: [[]] };
  const graph = stageGraphGeometry({ ...wide, latency: buckets(5, 200) });
  expect(graph.dots.map(({ lo, hi, t0, t1 }) => [lo, hi, t0, t1])).toEqual([
    [10, 15, 1000, 1400],
    [12, 17, 1400, 1800],
    [14, 18, 1800, 2000],
  ]);
  const next = stageGraphGeometry({ ...wide, latency: buckets(6, 200) });
  expect(next.dots.slice(0, 2)).toEqual(graph.dots.slice(0, 2));
  expect(next.dots[2]).toMatchObject({ lo: 14, hi: 19, t0: 1800, t1: 2200 });
});

test("a bar over the track's top is clamped at the edge and marked", () => {
  const graph = stageGraphGeometry({
    ...base,
    lanes: [[]],
    latency: [
      { t: 3100, t0: 3000, t1: 3200, ms: 20, lo: 10, hi: 30 },
      { t: 3300, t0: 3200, t1: 3400, ms: 40, lo: 10, hi: 500 },
    ],
  });
  expect(graph.dots[0]).toMatchObject({ ms: 20, over: false });
  expect(graph.dots[1]).toMatchObject({ yHi: 1.5, over: true });
});
