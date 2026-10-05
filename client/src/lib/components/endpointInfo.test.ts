import { test, expect } from "bun:test";
import type { TransportDiscovery } from "../runner/contract";
import { advertisedServerHttpPaths, serverLoadSummary } from "./endpointInfo";

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

test("occupancy reads as slots, cautions only past half and needs a slot limit", () => {
  for (const [pool, expected] of [
    [{ active: 0, max: 4 }, "0 of 4 slots"],
    [{ active: 1, max: 2 }, "1 of 2 slots"],
    [
      { active: 3, max: 4 },
      "3 of 4 slots, server busy: results may be affected",
    ],
    [{ active: 0, max: 0 }, null],
    [undefined, null],
  ] as const)
    expect(serverLoadSummary(pool)).toBe(expected);
});

test("HTTP capability paths do not claim a latency-only WebTransport target", () => {
  expect(
    advertisedServerHttpPaths({
      ...discovery(),
      throughput: {},
      latency: {
        "https://server.example": {
          state: "advertised",
          targets: [
            { transport: "webtransport", protocol: "http3", tls: true },
          ],
        },
      },
    } as unknown as TransportDiscovery),
  ).toEqual([]);
});
