import { expect, test } from "bun:test";
import { DEFAULT_CONFIG } from "../../app/preferences";
import { activities, type Activity } from "../run/schedule";
import type { HttpProtocol, TestConfig } from "../types";
import { describeStreams, planStreams, streamNotes, type PlanServer } from "./streams";

const ORIGIN = "https://meter.example";
const NEXT_HOP: Record<HttpProtocol, string | null> = {
  http1: "http/1.1",
  http2: "h2",
  http3: "h3",
  negotiated: null,
};

function server(
  name: string,
  protocol: HttpProtocol,
  { transport = "fetch-stream", origin = ORIGIN, socket = true, maxMeasurements = 32 } = {},
): PlanServer {
  const targets = socket ? [{ transport: "websocket" as const }] : [];
  return {
    server: { id: name, name },
    // The discovery origin is the transfer origin, over the same protocol.
    discovery: {
      origin,
      protocol: NEXT_HOP[protocol],
      maxMeasurements,
      latency: new Map([[origin, { blocked: null, targets }]]),
    },
    throughput: {
      target: { transport: transport as "fetch-stream" },
      fetch: { origin, protocol },
    },
  };
}
const stage = (transfer: Activity["transfer"], loadedLatency = true): Activity => ({
  stage: transfer.length === 2 ? "bidirectional" : transfer[0] === "down" ? "download" : "upload",
  transfer,
  loadedLatency,
});
const auto = (count: number): TestConfig["transferStreams"] => ({ mode: "auto", count });
const forced = (count: number): TestConfig["transferStreams"] => ({ mode: "forced", count });

const lanes = (servers: PlanServer[], activity: Activity, policy: TestConfig["transferStreams"]) =>
  servers.map(({ server }) => {
    const plan = planStreams(servers, activity, policy).get(server.id)!;
    return { down: plan.down.planned, up: plan.up.planned };
  });

test("one server's lanes: HTTP/1.1 keeps a control connection while it probes or uploads, as WebSockets hold none", () => {
  for (const [why, servers, activity, policy, expected] of [
    [
      "bidirectional HTTP/1.1 with loaded latency",
      [server("A", "http1")],
      stage(["down", "up"]),
      auto(6),
      { down: 3, up: 2 },
    ],
    [
      "unproven negotiated HTTP downloads as HTTP/1.1, with all six when nothing probes",
      [server("A", "negotiated")],
      stage(["down"], false),
      auto(6),
      { down: 6, up: 0 },
    ],
    [
      "an upload keeps one for its checkpoints",
      [server("A", "http1")],
      stage(["up"], false),
      auto(6),
      { down: 0, up: 5 },
    ],
    [
      "an upload without a WebSocket on its origin also keeps one for its HTTP feed",
      [server("A", "http1", { socket: false })],
      stage(["up"], false),
      auto(6),
      { down: 0, up: 4 },
    ],
    [
      "the ceiling bounds Automatic",
      [server("A", "http1")],
      stage(["down"], false),
      auto(1),
      { down: 1, up: 0 },
    ],
    [
      "HTTP/2 splits by direction",
      [server("A", "http2")],
      stage(["down", "up"]),
      auto(6),
      { down: 1, up: 4 },
    ],
    [
      "HTTP/3 keeps one each way",
      [server("A", "http3")],
      stage(["down", "up"]),
      auto(6),
      { down: 1, up: 1 },
    ],
    [
      "a WebTransport session runs one stream",
      [server("A", "http3", { transport: "webtransport" })],
      stage(["down", "up"]),
      auto(6),
      { down: 1, up: 1 },
    ],
    [
      "a session caps a forced count",
      [server("A", "http3", { transport: "webtransport" })],
      stage(["down"]),
      forced(20),
      { down: 16, up: 0 },
    ],
    [
      "forced counts over HTTP/2 are exact",
      [server("A", "http2")],
      stage(["down", "up"]),
      forced(12),
      { down: 12, up: 12 },
    ],
    [
      "the lease bounds a forced count beside an upload's feed and finalize",
      [server("A", "http2")],
      stage(["down", "up"]),
      forced(128),
      { down: 15, up: 15 },
    ],
    [
      "forced HTTP/1.1 stops at the pool",
      [server("A", "http1")],
      stage(["down"], false),
      forced(12),
      { down: 6, up: 0 },
    ],
    [
      "forced negotiated HTTP stops at what its pool runs beside its HTTP feed",
      [server("A", "negotiated")],
      stage(["down", "up"]),
      forced(8),
      { down: 2, up: 2 },
    ],
  ] as const)
    expect<unknown>(lanes([...servers], activity, policy)[0], why).toEqual(expected);
});

test("servers sharing an HTTP/1.1 origin share its pool, or the stage is refused", () => {
  const pair = [server("A", "http1"), server("B", "http1")];
  expect(lanes(pair, stage(["down"]), auto(4))).toEqual([
    { down: 3, up: 0 },
    { down: 2, up: 0 },
  ]);
  // Forced lanes stop at the shared pool.
  const trio = ["A", "B", "C"].map((name) => server(name, "http1"));
  expect(lanes(trio, stage(["down"], false), forced(3)).map(({ down }) => down)).toEqual([2, 2, 2]);
  // One connection serves both uploads' checkpoints.
  const uploads = [server("A", "http1"), server("B", "http3", { transport: "webtransport" })];
  expect(lanes(uploads, stage(["up"]), auto(6)).map(({ up }) => up)).toEqual([5, 1]);
  const crowd = ["A", "B", "C", "D"].map((name) => server(name, "http1"));
  for (const policy of [auto(4), forced(2)])
    expect(() => planStreams(crowd, stage(["down", "up"]), policy), policy.mode).toThrow(
      "A, B, C, D share one HTTP/1 origin without enough connections for this stage",
    );
});

test("an exact count says what holds it back: the browser's HTTP/1.1 connections or the lease", () => {
  const config = { ...DEFAULT_CONFIG, transferStreams: forced(12) };
  const http1 = server("S1", "http1");
  expect(streamNotes(config, [http1])).toEqual([
    "HTTP/1.1 allows at most 5 measurement connections to S1 in this browser; 5 will be used.",
  ]);
  expect(describeStreams(config.transferStreams, activities(config), http1).aside).toBe(
    "5 per direction (capped from 12 by the browser)",
  );
  expect(streamNotes(config, [server("S2", "http2", { maxMeasurements: 8 })])).toEqual([
    "S2 admits 8 measurements at once for this address or sign-in; as few as 6 streams per direction will be used.",
  ]);
  expect(streamNotes({ ...config, transferStreams: forced(4) }, [http1])).toEqual([]);
  const both = {
    ...config,
    stages: { ...config.stages, bidirectional: true },
    transferStreams: forced(4),
  };
  expect(streamNotes(both, [http1])).toEqual([
    "HTTP/1.1 allows at most 5 measurement connections to S1 in this browser; 5 will be used.",
  ]);
  expect(describeStreams(both.transferStreams, activities(both), http1).aside).toBe(
    "2 to 4 per direction (capped from 4 by the browser)",
  );
  expect(streamNotes({ ...config, transferStreams: auto(12) }, [http1])).toEqual([]);
});
