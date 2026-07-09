import { test, expect } from "bun:test";
import {
  canDisableBidirectional,
  latestBidirectionalLanes,
  latestOneWayThroughputForPhase,
} from "./stageGuards";
import type { ThroughputSample } from "../runner/contract";

test("canDisableBidirectional: freely toggleable while idle", () => {
  expect(canDisableBidirectional("idle", false)).toBe(true);
});

test("canDisableBidirectional: freely toggleable after complete", () => {
  expect(canDisableBidirectional("complete", false)).toBe(true);
});

test("canDisableBidirectional: toggleable while an earlier stage is running", () => {
  expect(canDisableBidirectional("download", true)).toBe(true);
  expect(canDisableBidirectional("upload", true)).toBe(true);
  expect(canDisableBidirectional("latency", true)).toBe(true);
  expect(canDisableBidirectional("warmup", true)).toBe(true);
});

test("canDisableBidirectional: locked while the bidirectional stage itself is running", () => {
  expect(canDisableBidirectional("bidirectional", true)).toBe(false);
});

function sample(
  phase: ThroughputSample["phase"],
  dir: ThroughputSample["dir"],
  bytesPerSec: number,
): ThroughputSample {
  return { t: 0, bytesPerSec, bytesCumulative: 0, dir, phase };
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
