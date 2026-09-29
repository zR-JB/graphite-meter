import { expect, test } from "bun:test";
import { replies, stageGraph } from "./stageGraph";
import { singleLatencyBucket } from "../runner/series";

test("a stage's replies are its measured buckets; a bucket of timeouts has no point", () => {
  const history = [
    singleLatencyBucket(100, 12, false, "latency"),
    singleLatencyBucket(300, 0, true, "latency"),
    singleLatencyBucket(500, 40, false, "download"),
    singleLatencyBucket(700, 14, false, "latency"),
  ];
  expect(replies(history, "latency")).toEqual([
    { t: 100, ms: 12 },
    { t: 700, ms: 14 },
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

test("a running stage spans its plan and its leading edge carries the glided rate", () => {
  const lane = [0, 1, 2].map((i) => ({ t: 1000 + i * 500, v: 50 }));
  const graph = stageGraph({
    ...base,
    lanes: [lane],
    head: { t: 3000, values: [100] },
  });
  expect(graph.heads).toEqual([{ x: 200, y: 3 }]);
  expect(graph.bins[0].at(-1)!.t).toBeLessThan(3000);
  expect(graph.area.endsWith("Z")).toBe(true);
});

test("a live lane ends at its head even when the newest bin is centred past it", () => {
  const graph = stageGraph({
    ...base,
    lanes: [[1000, 2000, 2961].map((t) => ({ t, v: 50 }))],
    head: { t: 2970, values: [80] },
  });
  const [{ x, y }] = graph.heads;
  expect(graph.bins[0].at(-1)!.t).toBeGreaterThan(2970);
  expect(graph.lines[0].endsWith(`${x} ${y}`)).toBe(true);
});

test("a reply below the idle median sits below its line; the track clamps at its top", () => {
  const graph = stageGraph({
    ...base,
    lanes: [[]],
    latency: [
      { t: 2000, ms: 5 },
      { t: 2000, ms: 30 },
      { t: 2000, ms: 500 },
      { t: 9000, ms: 30 },
    ],
  });
  const [below, above, capped] = graph.dots.map((dot) => dot.y);
  expect(graph.dots).toHaveLength(3);
  expect(below).toBeGreaterThan(graph.baselineY!);
  expect(above).toBeLessThan(graph.baselineY!);
  expect(capped).toBe(1.5);
  expect(graph.lines).toEqual([""]);
});
