import { test, expect } from "bun:test";
import type { TransportDiscovery } from "../runner/contract";
import {
  advertisedServerCapabilities,
  pathEvidence,
  serverLoadSummary,
  endpointPathStatus,
} from "./endpointInfo";

function discovery(): TransportDiscovery {
  return {
    generation: "test",
    engineVersion: "test",
    server: { name: "test" },
    fetchedAt: 0,
    pageOrigin: "https://app.example",
    pageSecure: true,
    throughput: {
      "https://server.example": {
        state: "advertised",
        targets: [{ transport: "fetch-stream" }],
      },
      "http://clear.example": {
        state: "browser-blocked",
        targets: [{ transport: "webtransport" }],
      },
    },
    latency: {
      "https://server.example": {
        state: "advertised",
        targets: [{ transport: "websocket" }],
      },
    },
  } as unknown as TransportDiscovery;
}

test("occupancy reads as slots and cautions only past half", () => {
  for (const [pool, expected] of [
    [{ active: 1, max: 2 }, "1 of 2 slots"],
    [
      { active: 3, max: 4 },
      "3 of 4 slots · server busy — results may be affected",
    ],
  ] as const)
    expect(serverLoadSummary(pool)).toBe(expected);
});

// A server with no measurement slots configured is neither idle nor busy: the ratio is not a number, so every.
test("a server with no slots configured reports no occupancy", () => {
  expect(serverLoadSummary({ active: 0, max: 0 })).toBeNull();
  expect(serverLoadSummary(undefined)).toBeNull();
});

// An idle server still has occupancy to report: only a missing pool drops the row, so the guard above cannot be.
test("an idle server with a configured pool still reports its slots", () => {
  expect(serverLoadSummary({ active: 0, max: 4 })).toBe("0 of 4 slots");
});

test("primary capabilities come from this server's discovery, not the runner", () => {
  expect(advertisedServerCapabilities(discovery(), "throughput")).toEqual({
    transports: ["fetch-stream", "webtransport"],
    browserBlocked: true,
  });
  expect(advertisedServerCapabilities(discovery(), "latency")).toEqual({
    transports: ["websocket"],
    browserBlocked: false,
  });
  expect(advertisedServerCapabilities(null, "latency")).toBeNull();
});

test("path evidence retains each protocol observation boundary", () => {
  for (const [mode, browser, server, expected] of [
    [
      "throughput",
      "h2",
      "http/1.1",
      "Browser observed HTTP/2 · Server observed HTTP/1.1",
    ],
    ["throughput", undefined, "h3", "Server observed HTTP/3"],
    ["latency", "h2", "http/1.1", "Server observed HTTP/1.1"],
    ["latency", undefined, undefined, "Pending"],
  ] as const)
    expect(pathEvidence(mode, browser, server)).toBe(expected);
});

test("endpoint path status describes verified paths by mode", () => {
  for (const [mode, expected] of [
    ["live", { label: "Ready", tone: "ready" }],
    ["running", { label: "In use", tone: "active" }],
    ["result", { label: "Used", tone: "used" }],
  ] as const)
    expect(endpointPathStatus("verified", mode)).toEqual(expected);
});

test("endpoint path status keeps non-verified validation truthful", () => {
  for (const [validation, mode, expected] of [
    ["checking", "result", { label: "Checking", tone: "checking" }],
    ["failed", "running", { label: "Failed", tone: "failed" }],
  ] as const)
    expect(endpointPathStatus(validation, mode)).toEqual(expected);
});
