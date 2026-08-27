import { expect, test } from "bun:test";
import type { FetchThroughputTarget, WebSocketLatencyTarget } from "../api/endpoints";
import type { InfraInfo, RunnerConfig, TransportDiscovery } from "./contract";
import {
  roleNeedsValidation,
  connectionKey,
  connectionDraftKey,
  connectionDraftRoleKey,
  connectionRoleKey,
  validationRoles,
  verifiedRolesForProbe,
  presentConnections,
  type ConnectionValidation,
  panelReadiness,
  type ConnectionValidationState,
} from "./connectionModel";
import { classifyTransportDiscovery, ROUTES } from "./real/backendPure";
import { DEFAULT_CONFIG } from "../state/defaults";

const throughput: FetchThroughputTarget = {
  id: "http2",
  origin: "https://meter.test",
  transport: "fetch-stream",
  protocol: "http2",
  tls: true,
  routes: {
    probe: ROUTES.probe,
    download: ROUTES.download,
    upload: ROUTES.upload,
    uploadSession: ROUTES.uploadSession,
    uploadProgress: ROUTES.uploadProgress,
  },
};
const latency: WebSocketLatencyTarget = {
  id: "ws-http1-tls",
  origin: "https://meter.test:7247",
  transport: "websocket",
  protocol: "http1",
  tls: true,
  routes: { probe: ROUTES.probe, ping: ROUTES.ping },
};
const webTransportOnly = {
  baseUrl: "https://wt.test",
  transport: "webtransport" as const,
  protocol: "http3" as const,
};
function config(): RunnerConfig {
  return {
    ...structuredClone(DEFAULT_CONFIG),
    transferStreams: { mode: "auto", count: 6 },
  };
}
type DiscoveryOptions = {
  throughput?: Parameters<typeof classifyTransportDiscovery>[0];
  latency?: Parameters<typeof classifyTransportDiscovery>[1];
  pageOrigin?: string;
  pageSecure?: boolean;
  pageProtocol?: string;
};
function makeDiscovery(options: DiscoveryOptions = {}): TransportDiscovery {
  return {
    ...classifyTransportDiscovery(
      options.throughput ?? [throughput],
      options.latency ?? [latency],
      options.pageOrigin ?? "https://meter.test",
      options.pageSecure ?? true,
      options.pageProtocol ?? "h2",
    ),
    generation: "generation-a",
    engineVersion: "test",
    server: { name: "meter" },
    fetchedAt: 1,
  };
}
type ValidationOverrides = {
  throughput?: Partial<ConnectionValidation["throughput"]>;
  latency?: Partial<ConnectionValidation["latency"]>;
};
function makeValidation(overrides: ValidationOverrides = {}): ConnectionValidation {
  return {
    throughput: { selection: "auto", state: "stale", ...overrides.throughput },
    latency: { selection: "auto", state: "stale", ...overrides.latency },
  };
}
function makeInfra(discovery: TransportDiscovery = makeDiscovery(), overrides: Partial<InfraInfo> = {}): InfraInfo {
  const { server, ...rest } = overrides;
  return {
    clientIp: "192.0.2.2",
    clientIpVersion: 4,
    clientIpSource: "socket",
    server: { ...(server ?? discovery.server) },
    preTestPingMs: 0,
    engineVersion: "test",
    discoveryGeneration: discovery.generation,
    protocolNegotiated: "h2",
    ...rest,
  };
}
function present(cfg = config(), discovery = makeDiscovery(), validation = makeValidation(), infra: InfraInfo | null = makeInfra(discovery)) {
  return presentConnections(cfg, discovery, validation, infra);
}
function degradedThroughput(protocol: "http2" | "http3") {
  const cfg = config();
  const discovery = makeDiscovery({
    throughput: [webTransportOnly],
    latency: [],
    pageOrigin: "https://ui.test",
  });
  const validation = makeValidation({
    throughput: {
      identity: connectionRoleKey(cfg, "throughput", discovery),
      state: "verified",
      verifiedAt: 2,
    },
  });
  const infra = makeInfra(discovery, {
    protocolNegotiated: protocol === "http2" ? "h2" : "h3",
    selectedThroughputTarget: "https://wt.test",
    selectedThroughputProtocol: protocol,
    selectedThroughputTransport: "fetch-stream",
  });
  return present(cfg, discovery, validation, infra).throughput;
}
function withWebTransport(run: () => void): void {
  const globals = globalThis as Record<string, unknown>;
  const previous = globals.WebTransport;
  globals.WebTransport = class {};
  try {
    run();
  } finally {
    if (previous === undefined) Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = previous;
  }
}
test("presentation keeps browser and server protocol boundaries distinct", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  cfg.transports.latencyTarget = latency.origin;
  const discovery = makeDiscovery();
  const validation = makeValidation({
    throughput: {
      selection: throughput.origin,
      state: "verified",
      verifiedAt: 2,
    },
    latency: { selection: latency.origin, state: "verified", verifiedAt: 2 },
  });
  const infra = makeInfra(discovery, {
    clientIpSource: "forwarded",
    preTestPingMs: 4,
    protocolNegotiated: "http/1.1",
    firstHopProtocol: "h2",
    latencyProtocolNegotiated: "http/1.1",
  });

  const model = present(cfg, discovery, validation, infra);

  expect(model.throughput.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(model.throughput.browserProtocol).toBe("h2");
  expect(model.throughput.serverProtocol).toBe("http/1.1");
  expect(model.latency.summary).toBe("WebSocket · TLS");
  expect(model.latency.preTestPingMs).toBe(4);
});
test("native H1 latency summary states its deterministic HTTP version", () => {
  const cfg = config();
  const direct = { ...latency, origin: throughput.origin, tls: true };
  const discovery = makeDiscovery({
    throughput: [{ ...throughput, protocol: "http1" }],
    latency: [direct],
    pageProtocol: "http/1.1",
  });
  const validation = makeValidation({
    latency: { state: "verified", verifiedAt: 2 },
  });
  const infra = makeInfra(discovery, {
    latencyClientIp: "192.0.2.2",
    latencyClientIpVersion: 4,
    latencyClientIpSource: "socket",
    preTestPingMs: 4,
    protocolNegotiated: "http/1.1",
    latencyProtocolNegotiated: "http/1.1",
  });
  expect(present(cfg, discovery, validation, infra).latency.summary).toBe("WebSocket · HTTP/1.1 · TLS");
});
test("verified negotiated throughput presents the observed browser protocol", () => {
  const cfg = config();
  const discovery = makeDiscovery();
  discovery.throughput[throughput.origin].targets[0].protocol = "negotiated";
  const validation = makeValidation({
    throughput: { state: "verified", verifiedAt: 2 },
  });
  const infra = makeInfra(discovery, {
    clientIpSource: "forwarded",
    protocolNegotiated: "http/1.1",
    selectedThroughputProtocol: "http2",
    firstHopProtocol: "h2",
  });

  const presented = present(cfg, discovery, validation, infra).throughput;
  expect(presented.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(presented.observedProtocol).toBe("http2");
});
test("old evidence never appears under a new selection or generation", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  const discovery = makeDiscovery();
  const validation = makeValidation({
    throughput: { selection: "http1-clear", state: "verified", verifiedAt: 2 },
  });
  const infra = makeInfra(discovery, {
    preTestPingMs: 4,
    discoveryGeneration: "old",
    protocolNegotiated: "h2",
  });

  expect(present(cfg, discovery, validation, infra).throughput.serverProtocol).toBeUndefined();
  expect(present(cfg, discovery, validation, infra).latency.preTestPingMs).toBeUndefined();
});
test("connection cache key changes only for preparation inputs", () => {
  const a = config();
  const b = config();
  b.visualization.throughputMaxBytesPerSec = 1_000_000;
  expect(connectionKey(a)).toBe(connectionKey(b));
  b.transports.latencyTarget = "http://meter.test";
  expect(connectionKey(a)).not.toBe(connectionKey(b));
});
test("role cache keys isolate throughput from latency preparation", () => {
  const a = config();
  const b = config();
  b.transports.throughputTarget = throughput.origin;
  expect(connectionRoleKey(a, "latency")).toBe(connectionRoleKey(b, "latency"));
  expect(connectionRoleKey(a, "throughput")).not.toBe(connectionRoleKey(b, "throughput"));
});
test("automatic and explicit selections share an identity when they resolve to the same target", () => {
  const automatic = config();
  const explicit = config();
  explicit.transports.throughputTarget = throughput.origin;
  explicit.transports.latencyTarget = latency.origin;
  const discovery = makeDiscovery();

  expect(connectionRoleKey(automatic, "throughput", discovery)).toBe(connectionRoleKey(explicit, "throughput", discovery));
  expect(connectionRoleKey(automatic, "latency", discovery)).toBe(connectionRoleKey(explicit, "latency", discovery));
  expect(connectionKey(automatic, discovery)).toBe(connectionKey(explicit, discovery));
});
test("draft invalidation ignores discovery and observed protocol changes", () => {
  const cfg = config();
  const discovery = makeDiscovery();
  const roleKey = connectionDraftRoleKey(cfg, "throughput");
  const key = connectionDraftKey(cfg);

  discovery.throughput[throughput.origin].targets[0].protocol = "http2";
  expect(connectionDraftRoleKey(cfg, "throughput")).toBe(roleKey);
  expect(connectionDraftKey(cfg)).toBe(key);

  cfg.transports.throughputTarget = throughput.origin;
  expect(connectionDraftRoleKey(cfg, "throughput")).not.toBe(roleKey);
  expect(connectionDraftKey(cfg)).not.toBe(key);
});
test("probe failure and stale evidence remain retryable presentation states", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  const failed = makeValidation({
    throughput: {
      selection: throughput.origin,
      state: "failed",
      message: "probe timed out",
    },
  });

  const model = present(cfg, makeDiscovery(), failed, null);
  expect(model.throughput.availability).toBe("advertised");
  expect(model.throughput.validation).toBe("failed");
  expect(model.throughput.message).toBe("probe timed out");
  expect(model.latency.validation).toBe("stale");
});
test("a ::wt selection shares its origin's availability", () => {
  const cfg = config();
  cfg.transports.throughputTarget = "https://meter.test::wt";
  const discovery = makeDiscovery({
    throughput: [
      throughput,
      {
        baseUrl: "https://meter.test",
        transport: "webtransport",
        protocol: "http3",
      },
    ],
  });
  const validation = makeValidation({
    throughput: { selection: "https://meter.test::wt", state: "checking" },
    latency: { state: "checking" },
  });

  const model = present(cfg, discovery, validation, null);
  expect(model.throughput.availability).toBe("advertised");
  expect(model.throughput.target).toBeNull();

  withWebTransport(() => {
    const driveable = present(cfg, discovery, validation, null);
    expect(driveable.throughput.availability).toBe("advertised");
    expect(driveable.throughput.target?.transport).toBe("webtransport");
  });
});
test("the panel names the latency bus the probe committed to, not the preferred one", () => {
  withWebTransport(() => {
    const cfg = config();
    const discovery = makeDiscovery({
      latency: [
        { baseUrl: throughput.origin, transport: "websocket" },
        { baseUrl: throughput.origin, transport: "webtransport" },
      ],
    });
    expect(connectionRoleKey(cfg, "latency", discovery)).toContain("::wt");
    const validation = makeValidation({
      latency: {
        identity: connectionRoleKey(cfg, "latency", discovery),
        state: "verified",
        verifiedAt: 2,
      },
    });
    const infra = makeInfra(discovery, {
      preTestPingMs: 4,
      protocolNegotiated: "h2",
      selectedLatencyTarget: throughput.origin,
      selectedLatencyTransport: "websocket",
    });

    const presented = present(cfg, discovery, validation, infra).latency;
    expect(presented.target?.transport).toBe("websocket");
    expect(presented.summary).toBe("WebSocket · TLS");
  });
});
test("a throughput role degraded off its session target presents the fetch view", () => {
  const presented = degradedThroughput("http3");
  expect(presented.target?.transport).toBe("fetch-stream");
  expect(presented.summary).toBe("Fetch stream · HTTP/3 · TLS");
});
test("a degraded throughput role names the protocol its fetch view negotiated", () => {
  const presented = degradedThroughput("http2");
  expect(presented.target?.transport).toBe("fetch-stream");
  expect(presented.observedProtocol).toBe("http2");
  expect(presented.summary).toBe("Fetch stream · HTTP/2 · TLS");
});
test("the panel resolves no throughput path this browser cannot drive", () => {
  const cfg = config();
  const discovery = makeDiscovery({
    throughput: [webTransportOnly],
    latency: [],
    pageOrigin: "https://ui.test",
  });
  const validation = makeValidation({
    throughput: { state: "checking" },
  });

  const presented = present(cfg, discovery, validation, null).throughput;
  expect(presented.target).toBeNull();
  expect(presented.availability).toBe("not-advertised");
  expect(presented.summary).toBe("Selection unresolved");
});
test("validation retries only the changed role and carries an aborted stale role", () => {
  const cfg = config();
  const validation = makeValidation({
    throughput: { state: "verified" },
    latency: { state: "verified" },
  });
  cfg.transports.throughputTarget = throughput.origin;
  expect(validationRoles(cfg, validation, "throughput")).toEqual(["throughput"]);

  validation.throughput = { selection: throughput.origin, state: "stale" };
  cfg.transports.latencyTarget = latency.origin;
  expect(validationRoles(cfg, validation, "latency")).toEqual(["latency", "throughput"]);
});
test("a generation refresh verifies every role checked by the broadened probe", () => {
  expect(verifiedRolesForProbe(["latency"], "old", "new")).toEqual(["throughput", "latency"]);
  expect(verifiedRolesForProbe(["latency"], "same", "same")).toEqual(["latency"]);
});
test("a stage toggle does not re-check a path that has not changed", () => {
  const cfg = config();
  const discovery = makeDiscovery();
  const verified = makeValidation({
    throughput: {
      selection: "auto",
      state: "verified",
      identity: connectionRoleKey(cfg, "throughput", discovery),
    },
    latency: {
      selection: "auto",
      state: "verified",
      identity: connectionRoleKey(cfg, "latency", discovery),
    },
  });

  expect(roleNeedsValidation(cfg, verified, "latency", discovery)).toBe(false);
  expect(roleNeedsValidation(cfg, verified, "throughput", discovery)).toBe(false);

  const off = config();
  off.stages.latency = false;
  off.stages.download = true;
  off.skipLoadedLatencyWhenStageOff = true;
  expect(roleNeedsValidation(off, verified, "latency", discovery)).toBe(false);

  const named = config();
  named.transports.latencyTarget = latency.origin;
  expect(roleNeedsValidation(named, verified, "latency", discovery)).toBe(false);
  const moved = config();
  moved.transports.latencyTarget = "https://elsewhere.test";
  expect(roleNeedsValidation(moved, verified, "latency", discovery)).toBe(true);

  const lost: ConnectionValidation = {
    ...verified,
    throughput: { selection: "auto", state: "stale" },
  };
  expect(roleNeedsValidation(cfg, lost, "throughput", discovery)).toBe(true);
});
test("the panel names a failure rather than reading as a check", () => {
  const present = (throughput: ConnectionValidationState, latency: ConnectionValidationState) =>
    ({
      throughput: { validation: throughput },
      latency: { validation: latency },
    }) as unknown as Parameters<typeof panelReadiness>[0];

  expect(panelReadiness(present("verified", "verified"), true)).toBe("verified");
  expect(panelReadiness(present("failed", "verified"), true)).toBe("failed");
  expect(panelReadiness(present("verified", "stale"), true)).toBe("stale");
  expect(panelReadiness(present("checking", "failed"), true)).toBe("failed");
  expect(panelReadiness(present("failed", "checking"), true)).toBe("failed");
  expect(panelReadiness(present("checking", "stale"), true)).toBe("checking");
  expect(panelReadiness(present("verified", "failed"), false)).toBe("verified");
});
