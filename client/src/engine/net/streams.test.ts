import { expect, test } from "bun:test";
import type { Activity } from "../run/schedule";
import type { HttpProtocol, TestConfig } from "../types";
import { planStreams, type PlanServer } from "./streams";

const ORIGIN = "https://meter.example";

function server(
  name: string,
  protocol: HttpProtocol,
  { transport = "fetch-stream", origin = ORIGIN, latency = ORIGIN as string | null } = {},
): PlanServer {
  return {
    server: { id: name, name },
    throughput: {
      target: { transport: transport as "fetch-stream" },
      fetch: { origin, protocol },
    },
    latency: latency === null ? null : { target: { transport: "websocket", origin: latency } },
  };
}
const stage = (transfer: Activity["transfer"], loadedLatency = true): Activity => ({
  stage: transfer.length === 2 ? "bidirectional" : transfer[0] === "down" ? "download" : "upload",
  transfer,
  loadedLatency,
});
const auto = (count: number): TestConfig["transferStreams"] => ({ mode: "auto", count });
const forced = (count: number): TestConfig["transferStreams"] => ({ mode: "forced", count });

/** Lists each server's lanes as [planned, gating] per direction. */
const lanes = (servers: PlanServer[], activity: Activity, policy: TestConfig["transferStreams"]) =>
  servers.map(({ server }) => {
    const plan = planStreams(servers, activity, policy).get(server.id)!;
    return { down: [plan.down.planned, plan.down.gating], up: [plan.up.planned, plan.up.gating] };
  });

test("one server's lanes: HTTP/1.1 shares its pool with the bus, the feed and checkpoints", () => {
  for (const [why, servers, activity, policy, expected] of [
    [
      "bidirectional HTTP/1.1 beside a WebSocket bus",
      [server("A", "http1")],
      stage(["down", "up"]),
      auto(6),
      { down: [2, 2], up: [1, 1] },
    ],
    [
      "unproven negotiated HTTP downloads as HTTP/1.1",
      [server("A", "negotiated")],
      stage(["down"]),
      auto(6),
      { down: [5, 5], up: [0, 0] },
    ],
    [
      "an HTTP upload keeps its feed and checkpoints",
      [server("A", "negotiated")],
      stage(["up"]),
      auto(6),
      { down: [0, 0], up: [3, 3] },
    ],
    [
      "a bus on another origin leaves the pool whole",
      [server("A", "http1", { latency: "https://bus.example" })],
      stage(["down"]),
      auto(6),
      { down: [6, 6], up: [0, 0] },
    ],
    [
      "the ceiling bounds Automatic",
      [server("A", "http1")],
      stage(["down"], false),
      auto(1),
      { down: [1, 1], up: [0, 0] },
    ],
    [
      "HTTP/2 splits by direction",
      [server("A", "http2")],
      stage(["down", "up"]),
      auto(6),
      { down: [1, 1], up: [4, 4] },
    ],
    [
      "HTTP/3 keeps one each way",
      [server("A", "http3")],
      stage(["down", "up"]),
      auto(6),
      { down: [1, 1], up: [1, 1] },
    ],
    [
      "a WebTransport session runs one stream",
      [server("A", "http3", { transport: "webtransport" })],
      stage(["down", "up"]),
      auto(6),
      { down: [1, 1], up: [1, 1] },
    ],
    [
      "a session caps a forced count",
      [server("A", "http3", { transport: "webtransport" })],
      stage(["down"]),
      forced(20),
      { down: [16, 16], up: [0, 0] },
    ],
    [
      "forced counts over HTTP/2 are exact",
      [server("A", "http2")],
      stage(["down", "up"]),
      forced(128),
      { down: [128, 128], up: [128, 128] },
    ],
    [
      "forced HTTP/1.1 past the pool gates on what runs",
      [server("A", "http1")],
      stage(["down"], false),
      forced(12),
      { down: [12, 6], up: [0, 0] },
    ],
    [
      "forced negotiated HTTP gates on what its pool runs",
      [server("A", "negotiated")],
      stage(["down", "up"]),
      forced(8),
      { down: [8, 2], up: [8, 1] },
    ],
  ] as const)
    expect<unknown>(lanes([...servers], activity, policy)[0], why).toEqual(expected);
});

test("servers sharing an HTTP/1.1 origin share its pool, or the stage is refused", () => {
  const pair = [server("A", "http1"), server("B", "http1")];
  expect(lanes(pair, stage(["down"]), auto(4))).toEqual([
    { down: [2, 2], up: [0, 0] },
    { down: [2, 2], up: [0, 0] },
  ]);
  // Forced lanes open exactly. Readiness waits only for those the shared pool runs.
  const trio = ["A", "B", "C"].map((name) => server(name, "http1", { latency: null }));
  expect(lanes(trio, stage(["down"]), forced(3)).map(({ down }) => down)).toEqual([
    [3, 2],
    [3, 2],
    [3, 2],
  ]);
  // One connection serves both uploads' checkpoints. Each HTTP feed keeps its own.
  const uploads = [
    server("A", "http1", { latency: null }),
    server("B", "http3", { transport: "webtransport", latency: null }),
  ];
  expect(lanes(uploads, stage(["up"]), auto(6)).map(({ up }) => up)).toEqual([
    [4, 4],
    [1, 1],
  ]);
  const crowd = ["A", "B", "C", "D"].map((name) => server(name, "http1"));
  for (const policy of [auto(4), forced(2)])
    expect(() => planStreams(crowd, stage(["down", "up"]), policy), policy.mode).toThrow(
      "A, B, C, D share one HTTP/1 origin without enough connections for this stage",
    );
});
