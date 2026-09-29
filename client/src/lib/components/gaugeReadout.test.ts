import { expect, test } from "bun:test";
import { gaugeReadout, type GaugeReadoutInput } from "./gaugeReadout";

const input = (overrides: Partial<GaugeReadoutInput>): GaugeReadoutInput => ({
  phase: "download",
  running: true,
  preparing: false,
  preparation: { status: "idle", throughput: "verified", latency: "verified" },
  startError: "",
  error: null,
  latencyTimeout: false,
  latencyMs: 12,
  quietMs: null,
  unansweredMs: null,
  replies: null,
  hasLatencyResult: false,
  unusable: false,
  headline: null,
  rate: (bytesPerSec) => String(bytesPerSec),
  unit: "B/s",
  ...overrides,
});

test("the display follows phase, evidence and missing data; a warmup shows none", () => {
  const upload = {
    phase: "bidirectional" as const,
    direction: "upload" as const,
    label: "Bidirectional upload",
    bytesPerSec: 50,
    dashed: true,
  };
  for (const [overrides, value, unit] of [
    [{ unusable: true }, "—", ""],
    [{ phase: "idle" }, "—", ""],
    [{ phase: "warmup" }, "—", ""],
    [{ phase: "latency" }, "12.0", "ms"],
    [{ phase: "latency", latencyTimeout: true }, "—", ""],
    [{ phase: "complete", hasLatencyResult: true }, "12.0", "ms"],
    [{ phase: "complete" }, "—", ""],
    [
      { phase: "complete", headline: upload },
      "50",
      "B/s · Bidirectional upload",
    ],
  ] as const)
    expect(gaugeReadout(input(overrides)).display).toEqual({ value, unit });
  expect(gaugeReadout(input({ phase: "download" })).display).toBeNull();
});

test("a stalled run states how long no data has arrived, in whole seconds", () => {
  expect(gaugeReadout(input({})).noData).toBe("");
  for (const [quietMs, note] of [
    [500, "No data for 1 s"],
    [4_200, "No data for 4 s"],
    [65_000, "No data for 1 min 5 s"],
  ] as const)
    expect(gaugeReadout(input({ quietMs })).noData).toBe(note);
});

test("unanswered latency probes count in the footer like a stall, not in the unit's place", () => {
  expect(gaugeReadout(input({ phase: "latency" })).noReplies).toBe("");
  const readout = gaugeReadout(
    input({ phase: "latency", latencyTimeout: true, unansweredMs: 3_400 }),
  );
  expect(readout.noReplies).toBe("No replies for 3 s");
  expect(readout.display).toEqual({ value: "—", unit: "" });
});

test("the latency stage counts its replies under the dial; other stages keep their hint", () => {
  expect(gaugeReadout(input({ phase: "latency", replies: 0 })).hint).toBe("");
  expect(gaugeReadout(input({ phase: "latency", replies: 1 })).hint).toBe(
    "1 reply",
  );
  expect(gaugeReadout(input({ phase: "latency", replies: 1234 })).hint).toBe(
    "1,234 replies",
  );
  expect(gaugeReadout(input({ phase: "download", replies: 12 })).hint).toBe("");
  expect(gaugeReadout(input({ phase: "warmup", replies: 12 })).hint).toBe(
    "Warmup",
  );
});

test("terminal readouts carry the measured direction and status", () => {
  const arc = {
    phase: "bidirectional" as const,
    direction: "download" as const,
    label: "Bidirectional download",
    bytesPerSec: 50,
    dashed: true,
  };
  const complete = gaugeReadout(input({ phase: "complete", headline: arc }));
  expect(complete.terminal?.direction).toBe("download");
  expect(gaugeReadout(input({ phase: "aborted" })).status?.error).toBe(false);
  expect(gaugeReadout(input({ phase: "error" })).status?.error).toBe(true);
});
