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
    [{ phase: "latency", latencyTimeout: true }, "—", "probe timeout"],
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
