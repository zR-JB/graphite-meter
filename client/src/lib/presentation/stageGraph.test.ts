import { expect, test } from "bun:test";
import { stageGraph } from "./stageGraph";

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

test("latency height is time added over the idle median, clamped to the track", () => {
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
  expect(graph.dots.map((dot) => dot.y)).toEqual([18.5, 10, 1.5]);
  expect(graph.lines).toEqual([""]);
});
