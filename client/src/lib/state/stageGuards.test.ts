import { test, expect } from "bun:test";
import {
  canDisableBidirectional,
  canToggleMeasuredStage,
  latestBidirectionalLanes,
  latestOneWayThroughputForPhase,
  sustainedRate,
  updateLiveThroughput,
} from "./stageGuards";
import type { ThroughputSample } from "../runner/contract";

test("canDisableBidirectional: freely toggleable while idle", () => {
  expect(canDisableBidirectional(null, false)).toBe(true);
});

test("canDisableBidirectional: freely toggleable after complete", () => {
  expect(canDisableBidirectional(null, false)).toBe(true);
});

test("canDisableBidirectional: toggleable while an earlier stage is running", () => {
  expect(canDisableBidirectional("download", true)).toBe(true);
  expect(canDisableBidirectional("upload", true)).toBe(true);
  expect(canDisableBidirectional("latency", true)).toBe(true);
  expect(canDisableBidirectional(null, true)).toBe(true);
});

test("canDisableBidirectional: locked while the bidirectional stage itself is running", () => {
  expect(canDisableBidirectional("bidirectional", true)).toBe(false);
});

test("measured stage guards use the warmup owner", () => {
  expect(canToggleMeasuredStage("latency", true, "latency")).toBe(false);
  expect(canToggleMeasuredStage("download", true, "latency")).toBe(true);
  expect(canToggleMeasuredStage("upload", true, "latency")).toBe(true);

  expect(canToggleMeasuredStage("latency", true, "download")).toBe(false);
  expect(canToggleMeasuredStage("download", true, "download")).toBe(false);
  expect(canToggleMeasuredStage("upload", true, "download")).toBe(true);

  expect(canToggleMeasuredStage("latency", true, "upload")).toBe(false);
  expect(canToggleMeasuredStage("download", true, "upload")).toBe(false);
  expect(canToggleMeasuredStage("upload", true, "upload")).toBe(false);
});

function sample(
  phase: ThroughputSample["phase"],
  dir: ThroughputSample["dir"],
  bytesPerSec: number,
): ThroughputSample {
  return {
    t: 0,
    bytesPerSec,
    bytesCumulative: 0,
    dir,
    phase,
    continuityId: 0,
  };
}

test("latestBidirectionalLanes: both zero when no samples yet", () => {
  expect(latestBidirectionalLanes([])).toEqual({ down: 0, up: 0 });
});

test("latestBidirectionalLanes: picks up the most recent sample per direction", () => {
  const samples = [
    sample("bidirectional", "down", 100),
    sample("bidirectional", "up", 50),
    sample("bidirectional", "down", 120),
  ];
  expect(latestBidirectionalLanes(samples)).toEqual({ down: 120, up: 50 });
});

test("latestBidirectionalLanes: one lane still zero if it hasn't reported yet", () => {
  const samples = [sample("bidirectional", "down", 200)];
  expect(latestBidirectionalLanes(samples)).toEqual({ down: 200, up: 0 });
});

test("latestBidirectionalLanes: stops scanning once it hits a differently-tagged sample", () => {
  const samples = [
    sample("download", "down", 999), // from a prior, unrelated phase
    sample("bidirectional", "down", 80),
  ];
  expect(latestBidirectionalLanes(samples)).toEqual({ down: 80, up: 0 });
});

test("latestBidirectionalLanes: treats zero as a real latest lane value", () => {
  const samples = [
    sample("bidirectional", "down", 100),
    sample("bidirectional", "up", 50),
    sample("bidirectional", "down", 0),
  ];
  expect(latestBidirectionalLanes(samples)).toEqual({ down: 0, up: 50 });
});

test("latestOneWayThroughputForPhase: ignores a previous transfer phase", () => {
  const samples = [sample("download", "down", 900)];
  expect(latestOneWayThroughputForPhase("upload", samples)).toBe(0);
});

test("latestOneWayThroughputForPhase: returns the active phase's latest sample", () => {
  const samples = [
    sample("download", "down", 900),
    sample("upload", "up", 500),
  ];
  expect(latestOneWayThroughputForPhase("upload", samples)).toBe(500);
});

test("live throughput keeps one current value per lane across long histories", () => {
  let live: ThroughputSample[] = [];
  live = updateLiveThroughput(live, sample("bidirectional", "up", 50));
  for (let rate = 1; rate <= 2_000; rate++)
    live = updateLiveThroughput(live, sample("bidirectional", "down", rate));
  expect(latestBidirectionalLanes(live)).toEqual({ down: 2_000, up: 50 });
  expect(updateLiveThroughput(live, sample("upload", "up", 25))).toEqual([
    sample("upload", "up", 25),
  ]);
});

test("sustained rate ignores a single spike and retains real values", () => {
  const samples = [100, 900, 100, 100].map((rate, index) => ({
    ...sample("download", "down", rate),
    t: index * 250,
  }));
  expect(sustainedRate(samples, 700)).toBe(100);
  expect(sustainedRate(samples, 200)).toBe(900);
});
