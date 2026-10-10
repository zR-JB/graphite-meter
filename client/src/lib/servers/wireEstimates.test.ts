import { expect, test } from "bun:test";
import { ThroughputAggregate, type MultiServerResult } from "../runner/measure";
import { headlineWire } from "./wireEstimates";
import { compensationTooltip, estimateCompensation } from "../compensation";
import { NOT_RUN } from "../runner/test-helpers.testutil";

function fixture(): MultiServerResult {
  const aggregate = new ThroughputAggregate();
  aggregate.begin("download", ["a", "b"], 0);
  aggregate.observe({ atMs: 0, down: { a: 0, b: 0 }, up: {} });
  aggregate.observe({ atMs: 1000, down: { a: 1000, b: 3000 }, up: {} });
  aggregate.result("download", false);
  const selection = ["a", "b"].map((id) => ({
    id,
    url: `https://${id}.example`,
    name: id,
  }));
  return {
    selection,
    participants: ["a", "b"],
    latencyFocus: "a",
    failures: [],
    omittedIntervals: 0,
    intervals: aggregate.intervals,
    servers: selection.map((server, i) => ({
      server,
      throughput: {
        origin: server.url,
        transport: "fetch-stream",
        protocol: i ? "http3" : "http1",
        browserProtocol: i ? "h3" : "http/1.1",
        clientIpVersion: i ? 6 : 4,
      },
      latencyTarget: null,
      latency: null,
      latencyByStage: {
        latency: null,
        download: null,
        upload: null,
        bidirectional: null,
      },
      addedLatency: null,
      download: null,
      upload: null,
      bidirectional: null,
      totalBytes: { down: 0, up: 0 },
      stages: NOT_RUN,
    })),
  };
}
const estimate = (
  details: MultiServerResult,
  stage: string,
  dir: "down" | "up",
) =>
  headlineWire(
    details.intervals.findLast((interval) => interval.stage === stage)
      ?.headline,
    dir,
    (id) =>
      details.servers.find((server) => server.server.id === id)?.throughput ??
      null,
  );

test("mixed transports estimate each simultaneous component with its own protocol and IP evidence", () => {
  const details = fixture();
  const wire = estimate(details, "download", "down")!;
  const a = estimateCompensation(1000, "http/1.1", true, 4, "fetch-stream");
  const b = estimateCompensation(3000, "h3", true, 6, "fetch-stream");
  expect(wire.measuredBytesPerSec).toBe(4000);
  expect(wire.estimatedBytesPerSec).toBe(
    a.estimatedBytesPerSec + b.estimatedBytesPerSec,
  );
  expect(wire.componentCount).toBe(2);
  // Each family keeps its row, weighted by its server's bytes, and the rows add up to the whole overhead.
  const ip = (e: typeof a, label: string) =>
    e.factors.find((factor) => factor.label === label)!.contributionPct;
  const tip = compensationTooltip(wire);
  expect(tip).toContain(`IPv4\t+${(ip(a, "IPv4") / 4).toFixed(2)}%`);
  expect(tip).toContain(`IPv6\t+${((ip(b, "IPv6") * 3) / 4).toFixed(2)}%`);
  expect(tip).toContain("Each server's headers, weighted by its bytes");
  const total = wire.factors.reduce(
    (sum, factor) => sum + factor.contributionPct,
    0,
  );
  expect(1 + total / 100).toBeCloseTo(wire.totalMultiplier, 6);
});
test("missing evidence or an insufficient final interval cannot reuse another component's wire estimate", () => {
  const details = fixture();
  delete details.servers[1].throughput.browserProtocol;
  expect(estimate(details, "download", "down")).toBeNull();
  details.servers[1].throughput.browserProtocol = "h3";
  delete details.servers[1].throughput.clientIpVersion;
  expect(estimate(details, "download", "down")).toBeNull();
  const short = fixture();
  short.intervals[0].headline!.down![1].durationMs = 500;
  expect(estimate(short, "download", "down")).toBeNull();
  expect(estimate(fixture(), "upload", "up")).toBeNull();
});
