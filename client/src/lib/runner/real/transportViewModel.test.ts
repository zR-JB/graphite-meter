import { expect, test } from "bun:test";
import type { FetchThroughputTarget, LatencyTarget } from "../../api/endpoints";
import { classifyTransportDiscovery } from "./backendPure";
import { latencyOptionView, throughputOptionView } from "./transportViewModel";

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
  expect(
    throughputOptionView(blocked, "http://meter.example:7246").detail,
  ).toContain("Advertised, but blocked from this secure page.");
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
  expect(throughputOptionView(catalog, "auto").detail).toBe(
    "Uses this page's connection.",
  );
  expect(latencyOptionView(catalog, "auto").detail).toBe(
    "Uses the same security as this page.",
  );
  expect(
    throughputOptionView({ ...catalog, pageOrigin: "https://proxy" }, "auto")
      .disabled,
  ).toBe(false);
});
