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

for (const [name, phase, running, expected] of [
  ["canDisableBidirectional: freely toggleable while idle", null, false, true],
  [
    "canDisableBidirectional: freely toggleable after complete",
    null,
    false,
    true,
  ],
  [
    "canDisableBidirectional: toggleable while an earlier stage is running",
    "download",
    true,
    true,
  ],
  [
    "canDisableBidirectional: toggleable while an earlier stage is running (upload)",
    "upload",
    true,
    true,
  ],
  [
    "canDisableBidirectional: toggleable while an earlier stage is running (latency)",
    "latency",
    true,
    true,
  ],
  [
    "canDisableBidirectional: toggleable while an earlier stage is running (idle)",
    null,
    true,
    true,
  ],
  [
    "canDisableBidirectional: locked while the bidirectional stage itself is running",
    "bidirectional",
    true,
    false,
  ],
] as const) {
  test(name, () =>
    expect(canDisableBidirectional(phase, running)).toBe(expected),
  );
}

test("measured stage guards use the warmup owner", () => {
  for (const [phase, owner, expected] of [
    ["latency", "latency", false],
    ["download", "latency", true],
    ["upload", "latency", true],
    ["latency", "download", false],
    ["download", "download", false],
    ["upload", "download", true],
    ["latency", "upload", false],
    ["download", "upload", false],
    ["upload", "upload", false],
  ] as const)
    expect(canToggleMeasuredStage(phase, true, owner)).toBe(expected);
});

const sample = (
  phase: ThroughputSample["phase"],
  dir: ThroughputSample["dir"],
  bytesPerSec: number,
): ThroughputSample => ({
  t: 0,
  bytesPerSec,
  bytesCumulative: 0,
  dir,
  phase,
  continuityId: 0,
});

for (const [name, samples, expected] of [
  [
    "latestBidirectionalLanes: both zero when no samples yet",
    [],
    { down: 0, up: 0 },
  ],
  [
    "latestBidirectionalLanes: picks up the most recent sample per direction",
    [
      sample("bidirectional", "down", 100),
      sample("bidirectional", "up", 50),
      sample("bidirectional", "down", 120),
    ],
    { down: 120, up: 50 },
  ],
  [
    "latestBidirectionalLanes: one lane still zero if it hasn't reported yet",
    [sample("bidirectional", "down", 200)],
    { down: 200, up: 0 },
  ],
  [
    "latestBidirectionalLanes: stops scanning once it hits a differently-tagged sample",
    [sample("download", "down", 999), sample("bidirectional", "down", 80)],
    { down: 80, up: 0 },
  ],
  [
    "latestBidirectionalLanes: treats zero as a real latest lane value",
    [
      sample("bidirectional", "down", 100),
      sample("bidirectional", "up", 50),
      sample("bidirectional", "down", 0),
    ],
    { down: 0, up: 50 },
  ],
] as const) {
  test(name, () => expect(latestBidirectionalLanes(samples)).toEqual(expected));
}

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
