import { test, expect } from "bun:test";
import {
  buildSegments,
  reconfigureTimeline,
  truncateSegmentAt,
} from "./schedule";
import type { RunnerConfig } from "./contract";
import { DEFAULT_CONFIG } from "../state/defaults";
const BASE_CONFIG: RunnerConfig = structuredClone(DEFAULT_CONFIG);
BASE_CONFIG.transferStreams = { mode: "auto", count: 6 };
BASE_CONFIG.transports = { throughputTarget: "current", latencyTarget: "auto" };
BASE_CONFIG.adaptive.enabled = false;
type ConfigOverrides = Omit<Partial<RunnerConfig>, "stages" | "duration"> & {
  stages?: Partial<RunnerConfig["stages"]>;
  duration?: Partial<RunnerConfig["duration"]>;
};
function cfg(overrides: ConfigOverrides = {}): RunnerConfig {
  return {
    ...BASE_CONFIG,
    ...overrides,
    stages: { ...BASE_CONFIG.stages, ...overrides.stages },
    duration: { ...BASE_CONFIG.duration, ...overrides.duration },
  };
}
test("bidirectional stage activity carries both directions in fixed order", () => {
  const c = cfg({ stages: { bidirectional: true } });
  const { segments } = buildSegments(c);
  const bidi = segments.find((s) => s.phase === "bidirectional");
  expect(bidi).toBeDefined();
  expect(bidi!.activity.transfer).toEqual(["down", "up"]);
  expect(bidi!.activity.stage).toBe("bidirectional");
});
test("bidirectional stage is omitted from the timeline when disabled", () => {
  const c = cfg({ stages: { bidirectional: false } });
  const { segments } = buildSegments(c);
  expect(segments.some((s) => s.phase === "bidirectional")).toBe(false);
});
test("bidirectional runs last, after latency/download/upload", () => {
  const c = cfg({ stages: { bidirectional: true } });
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
test.each([
  [
    "reconfigureTimeline appends a not-yet-started bidirectional stage when enabled mid-run",
    false,
    true,
  ],
  [
    "reconfigureTimeline drops a not-yet-started bidirectional stage when disabled mid-run",
    true,
    false,
  ],
])("%s", (_label, beforeEnabled, afterEnabled) => {
  const before = cfg({ stages: { bidirectional: beforeEnabled } });
  const { segments } = buildSegments(before);
  const after = cfg({ stages: { bidirectional: afterEnabled } });
  const rebuilt = reconfigureTimeline(segments, 0, after);
  expect(rebuilt.segments.some((s) => s.phase === "bidirectional")).toBe(
    afterEnabled,
  );
});
test("reconfigureTimeline resizes the active stage from its original start", () => {
  const before = cfg({
    stages: {
      latency: false,
      download: true,
      upload: false,
      bidirectional: false,
    },
    duration: { warmupMs: 0, downloadMs: 10000 },
  });
  const { segments } = buildSegments(before);
  const shortened = reconfigureTimeline(
    segments,
    3000,
    cfg({ stages: before.stages, duration: { downloadMs: 5000 } }),
  );
  expect(shortened.segments[0]).toMatchObject({ start: 0, end: 5000 });
  const expired = reconfigureTimeline(
    segments,
    3000,
    cfg({ stages: before.stages, duration: { downloadMs: 2000 } }),
  );
  expect(expired.totalMs).toBe(3000);
});
test("truncateSegmentAt closes at real elapsed and shifts the untouched tail", () => {
  const built = buildSegments(cfg({ duration: { warmupMs: 0 } }));
  const download = built.segments.find((s) => s.phase === "download")!;
  const elapsed = download.start + 5_500;
  const truncated = truncateSegmentAt(built.segments, download, elapsed);
  const next = truncated.segments.find((s) => s.phase === "upload")!;
  expect(truncated.segments.find((s) => s.phase === "download")!.end).toBe(
    elapsed,
  );
  expect(next.start).toBe(elapsed);
  expect(truncated.totalMs).toBe(built.totalMs - 4_500);
});
