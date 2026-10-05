import { test, expect } from "bun:test";
import {
  probeAccountingDetails,
  probeAccountingSummary,
  hasProbeAccountingNotice,
  formatTimeouts,
  profileDomain,
  timeoutsTip,
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

test("lanes take the gauge's ladder over their P90s, so one slow reply never sets the axis", () => {
  expect(profileDomain([lane(), lane({ min: 30, max: 60 })])).toBe(100);
  const lan = [
    lane({ p90: 0.5, max: 3.5 }),
    lane({ p90: 3, max: 45 }),
    lane({ p90: 1.2, max: 6 }),
  ];
  expect(profileDomain(lan)).toBe(4);
  expect(profileDomain([lane({ p90: null, center: 30 })])).toBe(40);
  expect(profileDomain([lane({ p90: 0.2, max: 0.4 })])).toBe(1);
});

test("timeouts read as a share of resolved probes, and the tip counts them", () => {
  expect(formatTimeouts(null)).toBe("—");
  expect(formatTimeouts(0)).toBe("0.0%");
  expect(formatTimeouts(0.0025)).toBe("0.25%");
  expect(formatTimeouts(0.034)).toBe("3.4%");
  expect(formatTimeouts(1)).toBe("100.0%");
  expect(timeoutsTip(lane({ count: 1200, timeoutCount: 3 }))).toBe(
    "Timeouts\n3 of 1,200 probes had no reply before the deadline\n" +
      "A timeout is a missing reply, not packet loss",
  );
  expect(timeoutsTip(lane({ count: 0, timeoutCount: null }))).toBe("");
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
