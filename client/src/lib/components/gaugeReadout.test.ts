import { expect, test } from "bun:test";
import { gaugeReadout, type GaugeReadoutInput } from "./gaugeReadout";

const input = (overrides: Partial<GaugeReadoutInput>): GaugeReadoutInput => ({
  phase: "download",
  running: true,
  preparing: false,
  preparation: { status: "idle", throughput: "ready", latency: "ready" },
  startError: "",
  error: null,
  aggregateEvidence: true,
  latencyTimeout: false,
  latencyMs: 12,
  hasLatencyResult: false,
  unusable: false,
  arcs: [],
  headline: null,
  animatedBytesPerSec: 100,
  measuredBytesPerSec: 200,
  rate: (bytesPerSec) => String(bytesPerSec),
  unit: "B/s",
  ...overrides,
});

test("the display follows phase, evidence and missing data", () => {
  const upload = {
    phase: "bidirectional" as const,
    direction: "upload" as const,
    label: "Bidirectional upload",
    bytesPerSec: 50,
    dashed: true,
  };
  for (const [overrides, value, unit] of [
    [{}, "100", "B/s"],
    [{ aggregateEvidence: false }, "—", "awaiting server windows"],
    [{ unusable: true }, "—", ""],
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
  expect(gaugeReadout(input({ phase: "error" })).status).toMatchObject({
    error: true,
    headline: "Something went wrong",
  });
});
