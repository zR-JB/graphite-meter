import { stubGlobals } from "../../test-helpers.test";
import { expect, test } from "bun:test";
import type { FetchThroughputTarget } from "../../api/endpoints";
import {
  classifyTransportDiscovery,
  targetOfKind,
  selectThroughputTarget,
  selectLatencyTarget,
} from "./backendPure";
import { latencyOptionView, throughputOptionView } from "./transportViewModel";
import { testLatency, testTransfer } from "../test-helpers.test";

test("IPv6 transport choices expose the DNS remedy and preserve exact same-origin targets", () => {
  const self = "http://[::1]:7246";
  const alternate = "http://[::1]:7247";
  const remote = classifyTransportDiscovery(
    [testTransfer("ipv6", self, "http1", false)],
    [testLatency("ipv6", self, false)],
    self,
    false,
    "http/1.1",
    "http://ui.example",
  );
  for (const selection of ["auto", self, "protocol:http1"])
    expect(throughputOptionView(remote, selection)).toEqual({
      disabled: true,
      detail: "Use a DNS hostname for browser connections to this IPv6 server.",
    });
  for (const selection of ["auto", self, "transport:websocket"])
    expect(latencyOptionView(remote, selection).detail).toContain(
      "DNS hostname",
    );
  expect(selectThroughputTarget(remote, "auto")).toBeNull();
  expect(selectLatencyTarget(remote, "auto")).toBeNull();

  const local = classifyTransportDiscovery(
    [
      testTransfer("self", self, "http1", false),
      testTransfer("alternate", alternate, "http1", false),
    ],
    [
      testLatency("self", self, false),
      testLatency("alternate", alternate, false),
    ],
    self,
    false,
  );
  expect(selectThroughputTarget(local, "auto")?.origin).toBe(self);
  expect(selectLatencyTarget(local, "auto")?.origin).toBe(self);
  expect(throughputOptionView(local, "auto").disabled).toBe(false);
  expect(throughputOptionView(local, alternate).detail).toContain(
    "DNS hostname",
  );
  expect(latencyOptionView(local, alternate).detail).toContain("DNS hostname");
});

test("status copy distinguishes missing, blocked, and trusted loopback targets", () => {
  const blocked = classifyTransportDiscovery(
    [testTransfer("http1-clear", "http://meter.example:7246", "http1", false)],
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
    [testTransfer("http1-clear", "http://localhost:7246", "http1", false)],
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
    [testTransfer("http2", "https://meter", "http2", true)],
    [testLatency("ws-http1-tls", "https://meter:7247", true)],
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

const NO_API = "WebTransport is unavailable in this browser.";
const INSECURE_PAGE =
  "Needs a secure page: browsers offer WebTransport over HTTPS only — reopen this page on its https:// address.";

/* Runs body with the page declaring itself insecure, which is what a browser that has the API does on an http://. */
function onAnInsecurePage(body: () => void) {
  const restore = stubGlobals({ isSecureContext: false });
  try {
    body();
  } finally {
    restore();
  }
}

test("WebTransport options disable in a browser without the API", () => {
  // bun's test environment has no WebTransport global, which is the case these views must catch before a probe fails.
  const catalog = classifyTransportDiscovery(
    [
      testTransfer("http3", "https://meter:7249", "http3", true),
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
    [testTransfer("http2", "https://a.example", "http2", true)],
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
    [testTransfer("http1-clear", "http://meter:7246", "http1", false)],
    [testLatency("ws-http1-clear", "http://meter:7246", false)],
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
    [testTransfer("proxy", "https://meter", "negotiated", true)],
    [testLatency("proxy", "https://meter", true)],
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
  const real = testTransfer("", "https://meter.example", "http3", true);
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
