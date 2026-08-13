import { test, expect } from "bun:test";
import type { TransportDiscovery } from "../runner/contract";
import {
  advertisedServerCapabilities,
  serverLoadSummary,
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
  expect(serverLoadSummary({ active: 1, max: 2 })).toBe("1 of 2 slots");
  expect(serverLoadSummary({ active: 3, max: 4 })).toBe(
    "3 of 4 slots · server busy — results may be affected",
  );
});

// A server with no measurement slots configured is neither idle nor busy: the
// ratio is not a number, so every comparison on it is false and the row would
// sit at "0 of 0 slots" for the life of the drawer. The row is dropped instead.
test("a server with no slots configured reports no occupancy", () => {
  expect(serverLoadSummary({ active: 0, max: 0 })).toBeNull();
  expect(serverLoadSummary(undefined)).toBeNull();
});

// An idle server still has occupancy to report: only a missing pool drops the
// row, so the guard above cannot be widened into "hide it when nobody is here".
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
