import { expect, test } from "bun:test";
import { AggregateMeasurements, type MultiServerResult } from "./measurement";
import { serverWireEstimate } from "./wireEstimates";
import { compensationTooltip, estimateCompensation } from "../compensation";

function fixture(): MultiServerResult {
  const aggregate = new AggregateMeasurements();
  aggregate.begin("download", ["a", "b"], 0);
  aggregate.observe({ atMs: 0, down: { a: 0, b: 0 }, up: {} });
  aggregate.observe({ atMs: 1000, down: { a: 1000, b: 3000 }, up: {} });
  aggregate.result("download", "down", false);
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
      bufferbloat: null,
      download: null,
      upload: null,
      bidirectional: null,
      totalBytes: { down: 0, up: 0 },
    })),
  };
}
test("mixed transports estimate each simultaneous component with its own protocol and IP evidence", () => {
  const details = fixture();
  const estimate = serverWireEstimate(details, "download", "down")!;
  const a = estimateCompensation(1000, "http/1.1", true, 4, "fetch-stream");
  const b = estimateCompensation(3000, "h3", true, 6, "fetch-stream");
  expect(estimate.measuredBytesPerSec).toBe(4000);
  expect(estimate.estimatedBytesPerSec).toBe(
    a.estimatedBytesPerSec + b.estimatedBytesPerSec,
  );
  expect(estimate.componentCount).toBe(2);
  expect(compensationTooltip(estimate)).toContain("2");
});
test("missing evidence or an insufficient final interval cannot reuse another component's wire estimate", () => {
  const details = fixture();
  delete details.servers[1].throughput.browserProtocol;
  expect(serverWireEstimate(details, "download", "down")).toBeNull();
  details.servers[1].throughput.browserProtocol = "h3";
  delete details.servers[1].throughput.clientIpVersion;
  expect(serverWireEstimate(details, "download", "down")).toBeNull();
  const short = fixture();
  short.intervals[0].headline!.down![1].durationMs = 500;
  expect(serverWireEstimate(short, "download", "down")).toBeNull();
  expect(serverWireEstimate(fixture(), "upload", "up")).toBeNull();
});
