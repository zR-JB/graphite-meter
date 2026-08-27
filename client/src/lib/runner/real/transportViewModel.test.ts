import { expect, test } from "bun:test";
import type { FetchThroughputTarget, LatencyTarget } from "../../api/endpoints";
import {
  classifyTransportDiscovery,
  ROUTES,
  targetOfKind,
} from "./backendPure";
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

const NO_API =
  "This browser has no WebTransport API. Chromium and Firefox have it; Safari does not.";
const INSECURE_PAGE =
  "Needs a secure page: browsers offer WebTransport over HTTPS only — reopen this page on its https:// address.";

/* Runs body with the page declaring itself insecure, which is what a browser that has the API does on an http://. */
function onAnInsecurePage(body: () => void) {
  const had = "isSecureContext" in globalThis;
  const previous = globalThis.isSecureContext;
  Object.defineProperty(globalThis, "isSecureContext", {
    value: false,
    configurable: true,
  });
  try {
    body();
  } finally {
    if (had)
      Object.defineProperty(globalThis, "isSecureContext", {
        value: previous,
        configurable: true,
      });
    else delete (globalThis as { isSecureContext?: boolean }).isSecureContext;
  }
}

test("WebTransport options disable in a browser without the API", () => {
  // bun's test environment has no WebTransport global, which is the case these views must catch before a probe fails.
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
  expect(wtThroughput.detail).toBe(NO_API);
  const wtLatency = latencyOptionView(catalog, "https://meter:7249");
  expect(wtLatency.disabled).toBe(true);
  expect(wtLatency.detail).toBe(NO_API);

  // Same missing global, different cause and different remedy: a page served over http is withheld the API a browser.
  onAnInsecurePage(() => {
    expect(throughputOptionView(catalog, "https://meter:7249::wt").detail).toBe(
      INSECURE_PAGE,
    );
    expect(latencyOptionView(catalog, "https://meter:7249").detail).toBe(
      INSECURE_PAGE,
    );
  });
});

// The automatic card resolves through the same selector the runner does, whose last resort is a WebTransport-only.
test("the automatic throughput card refuses a WebTransport-only origin", () => {
  const catalog = classifyTransportDiscovery(
    [
      {
        baseUrl: "https://wt.example:7249",
        transport: "webtransport" as const,
        protocol: "http3" as const,
      },
    ],
    [],
    "https://ui.example",
    true,
    "h2",
  );
  const automatic = throughputOptionView(catalog, "auto");
  expect(automatic.disabled).toBe(true);
  expect(automatic.detail).toBe(NO_API);
  onAnInsecurePage(() =>
    expect(throughputOptionView(catalog, "auto").detail).toBe(INSECURE_PAGE),
  );
});

// Nothing else advertised: the automatic card is unresolved for its own reason, not for a missing browser API.
test("an unresolved automatic throughput card still names its own reason", () => {
  const catalog = classifyTransportDiscovery(
    [transfer("http2", "https://a.example", "http2", true)],
    [],
    "https://ui.example",
    true,
    "h2",
  );
  catalog.throughput["https://a.example"].state = "browser-blocked";
  const automatic = throughputOptionView(catalog, "auto");
  expect(automatic.disabled).toBe(true);
  expect(automatic.detail).toBe(
    "No offered target matches this page origin and protocol.",
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

// An unrecognised mechanism is unvalidated JSON from a newer server.
test("an unknown transport is skipped, not renamed", () => {
  const unknown = {
    baseUrl: "https://meter.example",
    protocol: "http1",
    transport: "webtransport-v2",
  } as unknown as FetchThroughputTarget;
  const real = transfer("", "https://meter.example", "http3", true);
  const discovery = classifyTransportDiscovery(
    [unknown, real],
    [{ baseUrl: "https://meter.example", transport: "quic-ping" } as never],
    "https://meter.example",
    true,
  );
  const entry = discovery.throughput["https://meter.example"];
  expect(targetOfKind(entry, "fetch-stream")?.protocol).toBe("http3");
  expect(targetOfKind(entry, "webtransport")).toBeUndefined();
  expect(discovery.latency["https://meter.example"].targets).toEqual([]);
});

// Telling a browser without the API that the server offered nothing sends the reader after the wrong problem — the.
test("the automatic latency card names the browser gap, not the server", () => {
  const catalog = classifyTransportDiscovery(
    [],
    [{ baseUrl: "https://meter:7249", transport: "webtransport" as const }],
    "https://meter:7249",
    true,
    "h3",
  );
  expect(latencyOptionView(catalog, "auto").detail).toBe(NO_API);
  onAnInsecurePage(() =>
    expect(latencyOptionView(catalog, "auto").detail).toBe(INSECURE_PAGE),
  );
});
