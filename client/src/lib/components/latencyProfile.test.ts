import { test, expect } from "bun:test";
import {
  probeAccountingDetails,
  probeAccountingSummary,
  hasProbeAccountingNotice,
  entries,
  nearestMetric,
  hoverContext,
  metricLabel,
  profileDomain,
} from "./latencyProfile";
import type { LatencyLane } from "../state/store.svelte";

function lane(over: Partial<LatencyLane> = {}): LatencyLane {
  return {
    key: "latency",
    min: 10,
    max: 90,
    p10: 20,
    p90: 80,
    p95: null,
    center: 50,
    current: 55,
    jitter: 5,
    timeoutRatio: 0,
    accountingComplete: true,
    timeoutCount: 0,
    unresolvedCount: 0,
    sendFailureCount: 0,
    count: 100,
    active: false,
    ...over,
  };
}

test("profileDomain is shared by live and finalized lane profiles", () => {
  expect(profileDomain([lane(), lane({ min: 30, max: 60 })])).toEqual({
    min: 0,
    max: 200,
    span: 200,
  });
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
  expect(metricLabel("center")).toBe("Median");
  expect(hoverContext(l, "center")).toContain("Range");
  const result = lane({ center: 70 });
  expect(hoverContext(result, "current")).toBe("Median 70.0");
  expect(hoverContext(result, "center")).toContain("Range");
  expect(hoverContext(lane({ p10: null }), "p90")).toBe("");
  expect(hoverContext(lane({ center: null }), "current")).toBe("");
});

test("incomplete accounting stays visible without turning unknown outcomes into zero", () => {
  const incomplete = lane({
    accountingComplete: false,
    count: 0,
    timeoutCount: 0,
  });
  expect(hasProbeAccountingNotice(incomplete)).toBe(true);
  expect(probeAccountingDetails(incomplete)).toBe(
    "Known: 0 resolved · 0 timeouts · 0 unresolved · 0 send failures. Additional outcomes unknown.",
  );
  expect(hasProbeAccountingNotice(lane())).toBe(false);
  expect(hasProbeAccountingNotice(lane({ unresolvedCount: 2 }))).toBe(true);
});

test("visible probe counts emphasize replies and retain only nonzero exceptions", () => {
  expect(
    probeAccountingSummary(
      lane({
        count: 40,
        timeoutCount: 0,
        unresolvedCount: 0,
        sendFailureCount: 0,
      }),
    ),
  ).toEqual({ replies: "40 replies", exceptions: [] });
  expect(
    probeAccountingSummary(
      lane({
        count: 40,
        timeoutCount: 2,
        unresolvedCount: 3,
        sendFailureCount: 1,
      }),
    ),
  ).toEqual({
    replies: "38 replies",
    exceptions: ["2 timeouts", "3 unresolved", "1 send failure"],
  });
  expect(
    probeAccountingSummary(
      lane({
        count: 0,
        timeoutCount: null,
        unresolvedCount: null,
        sendFailureCount: null,
      }),
    ),
  ).toEqual({
    replies: "0 resolved",
    exceptions: [],
  });
});
