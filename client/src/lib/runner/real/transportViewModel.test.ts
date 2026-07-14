import { expect, test } from "bun:test";
import type { FetchThroughputTarget, LatencyTarget } from "../../api/preflight";
import { classifyTransportDiscovery } from "./backendPure";
import {
  latencyOptionView,
  testCombinationSummary,
  throughputOptionView,
} from "./transportViewModel";

const routes = {
  probe: "/probe",
  download: "/download",
  upload: "/upload",
  uploadSession: "/upload/session",
  uploadProgress: "/upload/progress",
};
const transfer = (
  id: string,
  origin: string,
  protocol: FetchThroughputTarget["protocol"],
  tls: boolean,
): FetchThroughputTarget => ({
  id,
  origin,
  protocol,
  tls,
  transport: "fetch-stream",
  routes,
});
const latency = (id: string, origin: string, tls: boolean): LatencyTarget => ({
  id,
  origin,
  protocol: "http1",
  tls,
  transport: "websocket",
  routes: { probe: "/probe", ping: "/ws/ping" },
});

test("status copy distinguishes missing, blocked, and trusted loopback targets", () => {
  const blocked = classifyTransportDiscovery(
    [transfer("http1-clear", "http://meter.example:7246", "http1", false)],
    [],
    "https://ui.example",
    true,
    "h2",
  );
  expect(throughputOptionView(blocked, "http1-clear").detail).toContain(
    "Advertised, but blocked from this secure page.",
  );
  expect(throughputOptionView(blocked, "http2").detail).toBe(
    "Not offered in /preflight.",
  );
  const loopback = classifyTransportDiscovery(
    [transfer("http1-clear", "http://localhost:7246", "http1", false)],
    [],
    "https://localhost:7247",
    true,
    "http/1.1",
  );
  expect(throughputOptionView(loopback, "http1-clear").detail).toBe(
    "Allowed as a browser-trusted loopback target.",
  );
});

test("dynamic cards report exact resolution or remain unresolved", () => {
  const catalog = classifyTransportDiscovery(
    [transfer("http2", "https://meter", "http2", true)],
    [latency("ws-http1-tls", "https://meter:7247", true)],
    "https://meter",
    true,
    "h2",
  );
  expect(throughputOptionView(catalog, "current").detail).toBe(
    "Resolved to HTTP/2 over TLS.",
  );
  expect(latencyOptionView(catalog, "auto").detail).toBe(
    "HTTPS page → secure WebSocket over HTTP/1.1.",
  );
  expect(
    throughputOptionView({ ...catalog, pageOrigin: "https://proxy" }, "current")
      .disabled,
  ).toBe(true);
});

test("combination summaries describe H1, H2, H3 and independent WS security", () => {
  const throughput = [
    transfer("http1-clear", "http://localhost:7246", "http1", false),
    transfer("http2", "https://localhost:7248", "http2", true),
    transfer("http3", "https://localhost:7249", "http3", true),
  ];
  const latencyTargets = [
    latency("ws-http1-clear", "http://localhost:7246", false),
    latency("ws-http1-tls", "https://localhost:7247", true),
  ];
  const catalog = classifyTransportDiscovery(
    throughput,
    latencyTargets,
    "http://localhost:7246",
    false,
    "http/1.1",
  );
  expect(testCombinationSummary(catalog, "current", "auto")).toEqual([
    "Throughput: HTTP/1.1 · clear · localhost:7246",
    "Latency: WebSocket · HTTP/1.1 clear · localhost:7246",
    "Upload progress follows the throughput path",
  ]);
  expect(testCombinationSummary(catalog, "http2", "ws-http1-tls")[0]).toBe(
    "Throughput: HTTP/2 · TLS · localhost:7248",
  );
  expect(testCombinationSummary(catalog, "http3", "ws-http1-tls")[0]).toBe(
    "Throughput: HTTP/3 · TLS · localhost:7249",
  );
});
