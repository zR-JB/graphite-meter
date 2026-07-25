import { expect, test } from "bun:test";
import type { FetchThroughputTarget, LatencyTarget } from "../../api/endpoints";
import { classifyTransportDiscovery, ROUTES } from "./backendPure";
import { latencyOptionView, throughputOptionView } from "./transportViewModel";

const routes = {
  probe: ROUTES.probe,
  download: ROUTES.download,
  upload: ROUTES.upload,
  uploadSession: ROUTES.uploadSession,
  uploadProgress: ROUTES.uploadProgress,
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
  routes: { probe: ROUTES.probe, ping: ROUTES.ping },
});

test("status copy distinguishes missing, blocked, and trusted loopback targets", () => {
  const blocked = classifyTransportDiscovery(
    [transfer("http1-clear", "http://meter.example:7246", "http1", false)],
    [],
    "https://ui.example",
    true,
    "h2",
  );
  expect(
    throughputOptionView(blocked, "http://meter.example:7246").detail,
  ).toBe(
    "Blocked by the browser: a secure page cannot open this clear endpoint · http://meter.example:7246",
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
  expect(throughputOptionView(loopback, "http://localhost:7246").detail).toBe(
    "Browser-trusted clear loopback endpoint · http://localhost:7246",
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
  expect(throughputOptionView(catalog, "auto").detail).toBe(
    "Selects https://meter because it matches this page.",
  );
  expect(latencyOptionView(catalog, "auto").detail).toBe(
    "Selects https://meter:7247 because it is the only available latency endpoint.",
  );
  expect(
    throughputOptionView({ ...catalog, pageOrigin: "https://proxy" }, "auto")
      .disabled,
  ).toBe(false);
});

test("WebTransport options disable in a browser without the API", () => {
  // bun's test environment has no WebTransport global, which is the case
  // these views must catch before a probe fails on it.
  const catalog = classifyTransportDiscovery(
    [
      transfer("http3", "https://meter:7249", "http3", true),
      {
        baseUrl: "https://meter:7249",
        transport: "webtransport" as const,
        protocol: "http3" as const,
      },
    ],
    [{ baseUrl: "https://meter:7249", transport: "webtransport" as const }],
    "https://meter:7249",
    true,
    "h3",
  );
  const wtThroughput = throughputOptionView(catalog, "https://meter:7249::wt");
  expect(wtThroughput.disabled).toBe(true);
  expect(wtThroughput.detail).toBe(
    "WebTransport is not supported by this browser.",
  );
  const wtLatency = latencyOptionView(catalog, "https://meter:7249");
  expect(wtLatency.disabled).toBe(true);
  expect(wtLatency.detail).toBe(
    "WebTransport is not supported by this browser.",
  );
});

test("endpoint copy distinguishes direct, negotiated, and WebSocket paths", () => {
  const direct = classifyTransportDiscovery(
    [transfer("http1-clear", "http://meter:7246", "http1", false)],
    [latency("ws-http1-clear", "http://meter:7246", false)],
    "http://meter:7246",
    false,
  );
  expect(throughputOptionView(direct, "http://meter:7246").detail).toBe(
    "Direct HTTP/1.1 endpoint · http://meter:7246",
  );
  expect(latencyOptionView(direct, "http://meter:7246").detail).toBe(
    "Direct HTTP/1.1 WebSocket endpoint · http://meter:7246",
  );

  const negotiated = classifyTransportDiscovery(
    [transfer("proxy", "https://meter", "negotiated", true)],
    [latency("proxy", "https://meter", true)],
    "https://meter",
    true,
  );
  expect(throughputOptionView(negotiated, "https://meter").detail).toBe(
    "Browser negotiates the available HTTP version · https://meter",
  );
  expect(latencyOptionView(negotiated, "https://meter").detail).toBe(
    "WebSocket endpoint · https://meter",
  );
});
