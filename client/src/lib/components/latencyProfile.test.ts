import { test, expect } from "bun:test";
import {
  pos,
  rangeWidth,
  tickLabel,
  lossLabel,
  entries,
  nearestMetric,
  hoverContext,
  metricLabel,
  profileDomain,
  savedLatencyLossVisible,
} from "./latencyProfile";
import type { LatencyLane } from "../state/store.svelte";

const DOMAIN = { min: 0, max: 100, span: 100 };

function lane(over: Partial<LatencyLane> = {}): LatencyLane {
  return {
    key: "latency",
    min: 10,
    max: 90,
    p10: 20,
    p90: 80,
    center: 50,
    centerKind: "average",
    current: 55,
    jitter: 5,
    lossRatio: 0,
    count: 100,
    active: false,
    ...over,
  };
}

test("pos: linear inside the domain, clamped at both ends, null at zero", () => {
  expect(pos(50, DOMAIN)).toBe(50);
  expect(pos(0, DOMAIN)).toBe(0);
  expect(pos(200, DOMAIN)).toBe(100); // above the domain clamps to full
  expect(pos(-10, DOMAIN)).toBe(0); // below the domain clamps to zero
  expect(pos(null, DOMAIN)).toBe(0);
});

test("rangeWidth: span as a percentage, hairline floor, null-safe", () => {
  expect(rangeWidth(10, 30, DOMAIN)).toBe(20);
  expect(rangeWidth(40, 40, DOMAIN)).toBe(1.5); // flat still shows
  expect(rangeWidth(null, 30, DOMAIN)).toBe(0);
  expect(rangeWidth(10, null, DOMAIN)).toBe(0);
});

test("profileDomain is shared by live and finalized lane profiles", () => {
  expect(profileDomain([lane(), lane({ min: 30, max: 60 })])).toEqual({
    min: 0,
    max: 200,
    span: 200,
  });
});

test("saved latency loss requires datagram transport evidence", () => {
  expect(savedLatencyLossVisible("webtransport-datagram")).toBe(true);
  expect(savedLatencyLossVisible("webtransport")).toBe(false);
  expect(savedLatencyLossVisible("websocket")).toBe(false);
  expect(savedLatencyLossVisible(null)).toBe(false);
});

test("tickLabel: non-positive collapses to a bare zero", () => {
  expect(tickLabel(0)).toBe("0");
  expect(tickLabel(-5)).toBe("0");
  expect(tickLabel(12)).not.toBe("0");
});

test("lossLabel: hidden at zero, extra precision under one percent", () => {
  expect(lossLabel(0)).toBe("");
  expect(lossLabel(-1)).toBe("");
  expect(lossLabel(0.005)).toBe("0.50% loss");
  expect(lossLabel(0.05)).toBe("5.0% loss");
});

test("entries: present metrics in label order, nulls dropped", () => {
  const got = entries(lane({ p10: null, current: null }));
  expect(got.map((e) => e.metric)).toEqual(["min", "center", "p90", "max"]);
});

test("nearestMetric: picks the closest measured value", () => {
  const l = lane();
  expect(nearestMetric(l, 51)).toBe("center"); // 50 is nearest
  expect(nearestMetric(l, 88)).toBe("max"); // 90 is nearest
  expect(nearestMetric(l, 0)).toBe("min"); // 10 is nearest
});

test("nearestMetric: no measured metrics yields null", () => {
  const empty = lane({
    min: null,
    max: null,
    p10: null,
    p90: null,
    center: null,
    current: null,
  });
  expect(nearestMetric(empty, 42)).toBeNull();
});

test("center labels and hover context follow the lane's semantics", () => {
  const l = lane();
  expect(hoverContext(l, "p10")).toContain("P10–P90");
  expect(metricLabel(l, "center")).toBe("Avg");
  expect(hoverContext(l, "center")).toContain("Range");
  expect(hoverContext(l, "current")).toContain("Avg");
  const result = lane({ center: 70, centerKind: "result" });
  expect(metricLabel(result, "center")).toBe("Result");
  expect(hoverContext(result, "current")).toBe("Result 70.0");
  expect(hoverContext(result, "center")).toContain("Range");
  expect(hoverContext(lane({ p10: null }), "p90")).toBe("");
  expect(hoverContext(lane({ center: null }), "current")).toBe("");
});
