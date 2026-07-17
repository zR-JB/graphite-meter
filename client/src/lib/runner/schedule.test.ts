import { test, expect } from "bun:test";
import { buildSegments, rebuildTail } from "./schedule";
import type { RunnerConfig } from "./contract";

// A minimal but complete RunnerConfig fixture. store.svelte.ts's DEFAULT_CONFIG
// can't be imported here — it's a .svelte.ts module that runs Svelte 5 rune
// calls ($state) at module scope, which don't exist outside the Svelte
// compiler/runtime, so bun:test would throw "$state is not defined" on import.
const BASE_CONFIG: RunnerConfig = {
  stages: { latency: true, download: true, upload: true, bidirectional: false },
  skipLoadedLatencyWhenStageOff: true,
  duration: {
    warmupMs: 800,
    latencyMs: 4000,
    downloadMs: 10000,
    uploadMs: 10000,
    bidirectionalMs: 10000,
  },
  pingCadence: "instant",
  loadedPingCadence: "medium",
  transferStreams: { mode: "auto", count: 6 },
  experimentalChunkedDownload: false,
  transports: { throughputTarget: "current", latencyTarget: "auto" },
  compensation: {
    profile: "lan",
    transport: "auto",
    params: {
      mtuBytes: 1500,
      ipVersion: 4,
      vlanTagged: false,
      tcpOptionsMinBytes: 0,
      tcpOptionsMaxBytes: 12,
      encapsulationBytes: 0,
      quicConnIdMinBytes: 0,
      quicConnIdMaxBytes: 20,
    },
  },
  adaptive: {
    enabled: false,
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
    glideMs: 1100,
  },
  visualization: { throughputMaxBytesPerSec: "auto" },
};

function cfg(overrides: Partial<RunnerConfig> = {}): RunnerConfig {
  return {
    ...BASE_CONFIG,
    ...overrides,
    stages: { ...BASE_CONFIG.stages, ...overrides.stages },
    duration: { ...BASE_CONFIG.duration, ...overrides.duration },
  };
}

test("bidirectional stage activity carries both directions in fixed order", () => {
  const c = cfg({ stages: { ...BASE_CONFIG.stages, bidirectional: true } });
  const { segments } = buildSegments(c);
  const bidi = segments.find((s) => s.phase === "bidirectional");
  expect(bidi).toBeDefined();
  expect(bidi!.activity.transfer).toEqual(["down", "up"]);
  expect(bidi!.activity.stage).toBe("bidirectional");
});

test("bidirectional stage is omitted from the timeline when disabled", () => {
  const c = cfg({ stages: { ...BASE_CONFIG.stages, bidirectional: false } });
  const { segments } = buildSegments(c);
  expect(segments.some((s) => s.phase === "bidirectional")).toBe(false);
});

test("bidirectional runs last, after latency/download/upload", () => {
  const c = cfg({ stages: { ...BASE_CONFIG.stages, bidirectional: true } });
  const { segments } = buildSegments(c);
  const bidiStart = segments.find((s) => s.phase === "bidirectional")!.start;
  for (const s of segments) {
    if (s.phase !== "bidirectional") expect(s.start).toBeLessThan(bidiStart);
  }
});

test("bidirectional gets its own warmup segment when warmupMs > 0", () => {
  const c = cfg({
    stages: {
      latency: false,
      download: false,
      upload: false,
      bidirectional: true,
    },
  });
  const { segments } = buildSegments(c);
  expect(segments.map((s) => s.phase)).toEqual(["warmup", "bidirectional"]);
});

test("rebuildTail appends a not-yet-started bidirectional stage when enabled mid-run", () => {
  const before = cfg({
    stages: { ...BASE_CONFIG.stages, bidirectional: false },
  });
  const { segments } = buildSegments(before);
  const after = cfg({
    stages: { ...BASE_CONFIG.stages, bidirectional: true },
  });
  const rebuilt = rebuildTail(segments, 0, after);
  expect(rebuilt.segments.some((s) => s.phase === "bidirectional")).toBe(true);
});

test("rebuildTail drops a not-yet-started bidirectional stage when disabled mid-run", () => {
  const before = cfg({
    stages: { ...BASE_CONFIG.stages, bidirectional: true },
  });
  const { segments } = buildSegments(before);
  const after = cfg({
    stages: { ...BASE_CONFIG.stages, bidirectional: false },
  });
  const rebuilt = rebuildTail(segments, 0, after);
  expect(rebuilt.segments.some((s) => s.phase === "bidirectional")).toBe(false);
});
