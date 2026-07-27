import { expect, test } from "bun:test";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/endpoints";
import type { InfraInfo, RunnerConfig } from "./contract";
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

function config(): RunnerConfig {
  return {
    stages: {
      latency: true,
      download: true,
      upload: true,
      bidirectional: false,
    },
    skipLoadedLatencyWhenStageOff: true,
    duration: {
      warmupMs: 800,
      latencyMs: 4000,
      downloadMs: 10000,
      uploadMs: 10000,
      bidirectionalMs: 10000,
    },
    pingCadence: "reply-driven",
    loadedPingCadence: "medium",
    transferStreams: { mode: "auto", count: 6 },
    experimentalChunkedDownload: false,
    experimentalDatagramThroughput: false,
    transports: { throughputTarget: "auto", latencyTarget: "auto" },
    compensation: {
      profile: "lan",
      transport: "auto",
      params: {
        mtuBytes: 1500,
        ipVersion: "auto",
        vlanTagged: false,
        tcpOptionsMinBytes: 0,
        tcpOptionsMaxBytes: 12,
        encapsulationBytes: 0,
        quicConnIdMinBytes: 0,
        quicConnIdMaxBytes: 20,
      },
    },
    adaptive: {
      enabled: true,
      minCoverageRatio: 0.52,
      stabilityThreshold: 0.86,
      maxPhaseReductionRatio: 0.5,
      minLatencySamples: 8,
      minTransferSamples: 12,
      glideMs: 1100,
    },
    visualization: { throughputMaxBytesPerSec: "auto" },
  };
}

function fixture() {
  return Object.assign(
    classifyTransportDiscovery(
      [throughput],
      [latency],
      "https://meter.test",
      true,
      "h2",
    ),
    {
      generation: "generation-a",
      engineVersion: "test",
      server: { name: "meter" },
      fetchedAt: 1,
    },
  );
}

test("presentation keeps browser and server protocol boundaries distinct", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  cfg.transports.latencyTarget = latency.origin;
  const validation: ConnectionValidation = {
    throughput: {
      selection: throughput.origin,
      state: "verified",
      verifiedAt: 2,
    },
    latency: { selection: latency.origin, state: "verified", verifiedAt: 2 },
  };
  const infra: InfraInfo = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4,
    clientIpSource: "forwarded",
    server: fixture().server,
    preTestPingMs: 4,
    engineVersion: "test",
    discoveryGeneration: "generation-a",
    protocolNegotiated: "http/1.1",
    firstHopProtocol: "h2",
    latencyProtocolNegotiated: "http/1.1",
  };

  const model = presentConnections(cfg, fixture(), validation, infra);

  expect(model.throughput.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(model.throughput.browserProtocol).toBe("h2");
  expect(model.throughput.serverProtocol).toBe("http/1.1");
  expect(model.latency.summary).toBe("WebSocket · TLS");
  expect(model.latency.preTestPingMs).toBe(4);
});

test("native H1 latency summary states its deterministic HTTP version", () => {
  const cfg = config();
  const direct = { ...latency, origin: throughput.origin, tls: true };
  const discovery = Object.assign(
    classifyTransportDiscovery(
      [{ ...throughput, protocol: "http1" }],
      [direct],
      throughput.origin,
      true,
      "http/1.1",
    ),
    {
      generation: "generation-a",
      engineVersion: "test",
      server: { name: "meter" },
      fetchedAt: 1,
    },
  );
  const validation: ConnectionValidation = {
    throughput: { selection: "auto", state: "stale" },
    latency: { selection: "auto", state: "verified", verifiedAt: 2 },
  };
  const infra: InfraInfo = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4,
    clientIpSource: "socket",
    latencyClientIp: "192.0.2.2",
    latencyClientIpVersion: 4,
    latencyClientIpSource: "socket",
    server: discovery.server,
    preTestPingMs: 4,
    engineVersion: "test",
    discoveryGeneration: discovery.generation,
    protocolNegotiated: "http/1.1",
    latencyProtocolNegotiated: "http/1.1",
  };
  expect(
    presentConnections(cfg, discovery, validation, infra).latency.summary,
  ).toBe("WebSocket · HTTP/1.1 · TLS");
});

test("verified negotiated throughput presents the observed browser protocol", () => {
  const cfg = config();
  const discovery = fixture();
  discovery.throughput[throughput.origin].targets[0].protocol = "negotiated";
  const validation: ConnectionValidation = {
    throughput: { selection: "auto", state: "verified", verifiedAt: 2 },
    latency: { selection: "auto", state: "stale" },
  };
  const infra: InfraInfo = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4,
    clientIpSource: "forwarded",
    server: discovery.server,
    preTestPingMs: 0,
    engineVersion: "test",
    discoveryGeneration: discovery.generation,
    protocolNegotiated: "http/1.1",
    selectedThroughputProtocol: "http2",
    firstHopProtocol: "h2",
  };

  const presented = presentConnections(
    cfg,
    discovery,
    validation,
    infra,
  ).throughput;
  expect(presented.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(presented.observedProtocol).toBe("http2");
});

test("old evidence never appears under a new selection or generation", () => {
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  const validation: ConnectionValidation = {
    throughput: { selection: "http1-clear", state: "verified", verifiedAt: 2 },
    latency: { selection: "auto", state: "stale" },
  };
  const infra = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4 as const,
    clientIpSource: "socket" as const,
    server: fixture().server,
    preTestPingMs: 4,
    engineVersion: "test",
    discoveryGeneration: "old",
    protocolNegotiated: "h2",
  };

  expect(
    presentConnections(cfg, fixture(), validation, infra).throughput
      .serverProtocol,
  ).toBeUndefined();
  expect(
    presentConnections(cfg, fixture(), validation, infra).latency.preTestPingMs,
  ).toBeUndefined();
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
  expect(connectionRoleKey(a, "throughput")).not.toBe(
    connectionRoleKey(b, "throughput"),
  );
});

test("automatic and explicit selections share an identity when they resolve to the same target", () => {
  const automatic = config();
  const explicit = config();
  explicit.transports.throughputTarget = throughput.origin;
  explicit.transports.latencyTarget = latency.origin;
  const discovery = fixture();

  expect(connectionRoleKey(automatic, "throughput", discovery)).toBe(
    connectionRoleKey(explicit, "throughput", discovery),
  );
  expect(connectionRoleKey(automatic, "latency", discovery)).toBe(
    connectionRoleKey(explicit, "latency", discovery),
  );
  expect(connectionKey(automatic, discovery)).toBe(
    connectionKey(explicit, discovery),
  );
});

test("draft invalidation ignores discovery and observed protocol changes", () => {
  const cfg = config();
  const discovery = fixture();
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
  const failed: ConnectionValidation = {
    throughput: {
      selection: throughput.origin,
      state: "failed",
      message: "probe timed out",
    },
    latency: { selection: "auto", state: "stale" },
  };

  const model = presentConnections(cfg, fixture(), failed, null);
  expect(model.throughput.availability).toBe("advertised");
  expect(model.throughput.validation).toBe("failed");
  expect(model.throughput.message).toBe("probe timed out");
  expect(model.latency.validation).toBe("stale");
});

test("a ::wt selection shares its origin's availability", () => {
  const cfg = config();
  cfg.transports.throughputTarget = "https://meter.test::wt";
  const discovery = Object.assign(
    classifyTransportDiscovery(
      [
        throughput,
        {
          baseUrl: "https://meter.test",
          transport: "webtransport" as const,
          protocol: "http3" as const,
        },
      ],
      [latency],
      "https://meter.test",
      true,
      "h2",
    ),
    {
      generation: "generation-a",
      engineVersion: "test",
      server: { name: "meter" },
      fetchedAt: 1,
    },
  );
  const validation: ConnectionValidation = {
    throughput: { selection: "https://meter.test::wt", state: "checking" },
    latency: { selection: "auto", state: "checking" },
  };

  // Availability describes the origin, so it is the same either way; the
  // resolved target additionally needs a browser that can drive the session.
  const model = presentConnections(cfg, discovery, validation, null);
  expect(model.throughput.availability).toBe("advertised");
  expect(model.throughput.target).toBeNull();

  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  globals.WebTransport = class {};
  try {
    const driveable = presentConnections(cfg, discovery, validation, null);
    expect(driveable.throughput.availability).toBe("advertised");
    expect(driveable.throughput.target?.transport).toBe("webtransport");
  } finally {
    if (realWebTransport === undefined)
      Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = realWebTransport;
  }
});

// A probe can commit to a transport the selector does not prefer: a WebTransport
// ping bus that never establishes is degraded to the origin's WebSocket bus.
// Naming the preferred mechanism and marking it verified reports a path the run
// never used, which is the silent transport change the role failure is there to
// prevent.
test("the panel names the latency bus the probe committed to, not the preferred one", () => {
  const globals = globalThis as Record<string, unknown>;
  const realWebTransport = globals.WebTransport;
  globals.WebTransport = class {};
  try {
    const cfg = config();
    const discovery = Object.assign(
      classifyTransportDiscovery(
        [throughput],
        [
          { baseUrl: throughput.origin, transport: "websocket" },
          { baseUrl: throughput.origin, transport: "webtransport" },
        ],
        throughput.origin,
        true,
        "h2",
      ),
      {
        generation: "generation-a",
        engineVersion: "test",
        server: { name: "meter" },
        fetchedAt: 1,
      },
    );
    // What the run wanted, and what it proved.
    expect(connectionRoleKey(cfg, "latency", discovery)).toContain("::wt");
    const validation: ConnectionValidation = {
      throughput: { selection: "auto", state: "stale" },
      latency: {
        selection: "auto",
        identity: connectionRoleKey(cfg, "latency", discovery),
        state: "verified",
        verifiedAt: 2,
      },
    };
    const infra: InfraInfo = {
      clientIp: "192.0.2.2",
      clientIpVersion: 4,
      clientIpSource: "socket",
      server: discovery.server,
      preTestPingMs: 4,
      engineVersion: "test",
      discoveryGeneration: discovery.generation,
      protocolNegotiated: "h2",
      selectedLatencyTarget: throughput.origin,
      selectedLatencyTransport: "websocket",
    };

    const presented = presentConnections(
      cfg,
      discovery,
      validation,
      infra,
    ).latency;
    expect(presented.target?.transport).toBe("websocket");
    expect(presented.summary).toBe("WebSocket · TLS");
  } finally {
    if (realWebTransport === undefined)
      Reflect.deleteProperty(globals, "WebTransport");
    else globals.WebTransport = realWebTransport;
  }
});

// The same degrade on the other role: a session target whose dial carries no
// bytes falls back to the origin's fetch view, which a WebTransport-only origin
// never advertised, so no discovery id names it.
test("a throughput role degraded off its session target presents the fetch view", () => {
  const cfg = config();
  const discovery = Object.assign(
    classifyTransportDiscovery(
      [
        {
          baseUrl: "https://wt.test",
          transport: "webtransport",
          protocol: "http3",
        },
      ],
      [],
      "https://ui.test",
      true,
      "h2",
    ),
    {
      generation: "generation-a",
      engineVersion: "test",
      server: { name: "meter" },
      fetchedAt: 1,
    },
  );
  const validation: ConnectionValidation = {
    throughput: {
      selection: "auto",
      identity: connectionRoleKey(cfg, "throughput", discovery),
      state: "verified",
      verifiedAt: 2,
    },
    latency: { selection: "auto", state: "stale" },
  };
  const infra: InfraInfo = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4,
    clientIpSource: "socket",
    server: discovery.server,
    preTestPingMs: 0,
    engineVersion: "test",
    discoveryGeneration: discovery.generation,
    protocolNegotiated: "h3",
    selectedThroughputTarget: "https://wt.test",
    selectedThroughputProtocol: "http3",
    selectedThroughputTransport: "fetch-stream",
  };

  const presented = presentConnections(
    cfg,
    discovery,
    validation,
    infra,
  ).throughput;
  expect(presented.target?.transport).toBe("fetch-stream");
  expect(presented.summary).toBe("Fetch stream · HTTP/3 · TLS");
});

// The fetch fallback off a session origin negotiates whatever the TCP path
// offers, which is not the HTTP/3 the session failed to reach. Carrying the
// session's protocol into the fetch view names the one protocol known to be
// wrong, while the drawer's observed-HTTP row reports the truth beside it.
test("a degraded throughput role names the protocol its fetch view negotiated", () => {
  const cfg = config();
  const discovery = Object.assign(
    classifyTransportDiscovery(
      [
        {
          baseUrl: "https://wt.test",
          transport: "webtransport",
          protocol: "http3",
        },
      ],
      [],
      "https://ui.test",
      true,
      "h2",
    ),
    {
      generation: "generation-a",
      engineVersion: "test",
      server: { name: "meter" },
      fetchedAt: 1,
    },
  );
  const validation: ConnectionValidation = {
    throughput: {
      selection: "auto",
      identity: connectionRoleKey(cfg, "throughput", discovery),
      state: "verified",
      verifiedAt: 2,
    },
    latency: { selection: "auto", state: "stale" },
  };
  const infra: InfraInfo = {
    clientIp: "192.0.2.2",
    clientIpVersion: 4,
    clientIpSource: "socket",
    server: discovery.server,
    preTestPingMs: 0,
    engineVersion: "test",
    discoveryGeneration: discovery.generation,
    protocolNegotiated: "h2",
    selectedThroughputTarget: "https://wt.test",
    selectedThroughputProtocol: "http2",
    selectedThroughputTransport: "fetch-stream",
  };

  const presented = presentConnections(
    cfg,
    discovery,
    validation,
    infra,
  ).throughput;
  expect(presented.target?.transport).toBe("fetch-stream");
  expect(presented.observedProtocol).toBe("http2");
  expect(presented.summary).toBe("Fetch stream · HTTP/2 · TLS");
});

// bun's test environment has no WebTransport global, which is exactly the
// browser the panel must not offer a session path to: the selector's last
// resort is a WebTransport-only origin, and every run there is refused.
test("the panel resolves no throughput path this browser cannot drive", () => {
  const cfg = config();
  const discovery = Object.assign(
    classifyTransportDiscovery(
      [
        {
          baseUrl: "https://wt.test",
          transport: "webtransport",
          protocol: "http3",
        },
      ],
      [],
      "https://ui.test",
      true,
      "h2",
    ),
    {
      generation: "generation-a",
      engineVersion: "test",
      server: { name: "meter" },
      fetchedAt: 1,
    },
  );
  const validation: ConnectionValidation = {
    throughput: { selection: "auto", state: "checking" },
    latency: { selection: "auto", state: "stale" },
  };

  const presented = presentConnections(
    cfg,
    discovery,
    validation,
    null,
  ).throughput;
  expect(presented.target).toBeNull();
  expect(presented.availability).toBe("not-advertised");
  expect(presented.summary).toBe("Selection unresolved");
});

test("validation retries only the changed role and carries an aborted stale role", () => {
  const cfg = config();
  const validation: ConnectionValidation = {
    throughput: { selection: "auto", state: "verified" },
    latency: { selection: "auto", state: "verified" },
  };
  cfg.transports.throughputTarget = throughput.origin;
  expect(validationRoles(cfg, validation, "throughput")).toEqual([
    "throughput",
  ]);

  validation.throughput = { selection: throughput.origin, state: "stale" };
  cfg.transports.latencyTarget = latency.origin;
  expect(validationRoles(cfg, validation, "latency")).toEqual([
    "latency",
    "throughput",
  ]);
});

test("a generation refresh verifies every role checked by the broadened probe", () => {
  expect(verifiedRolesForProbe(["latency"], "old", "new")).toEqual([
    "throughput",
    "latency",
  ]);
  expect(verifiedRolesForProbe(["latency"], "same", "same")).toEqual([
    "latency",
  ]);
});

// Toggling a stage is not a path change. A verified role stays verified, a
// latency path the run never opens is not checked at all, and only a role that
// resolves somewhere new or lost its verdict is worth a probe.
test("a stage toggle does not re-check a path that has not changed", () => {
  const cfg = config();
  const discovery = fixture();
  const verified: ConnectionValidation = {
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
  };

  // Latency on and verified: nothing to learn.
  expect(roleNeedsValidation(cfg, verified, "latency", discovery)).toBe(false);
  expect(roleNeedsValidation(cfg, verified, "throughput", discovery)).toBe(
    false,
  );

  // Latency off: the run never opens it, so it is not checked.
  const off = config();
  off.stages.latency = false;
  off.stages.download = true;
  off.skipLoadedLatencyWhenStageOff = true;
  expect(roleNeedsValidation(off, verified, "latency", discovery)).toBe(false);

  // Identity is the resolved target, not the selection string: naming the
  // origin that auto already picked resolves to the same place and is not
  // re-checked, while a selection that resolves elsewhere is.
  const named = config();
  named.transports.latencyTarget = latency.origin;
  expect(roleNeedsValidation(named, verified, "latency", discovery)).toBe(
    false,
  );
  const moved = config();
  moved.transports.latencyTarget = "https://elsewhere.test";
  expect(roleNeedsValidation(moved, verified, "latency", discovery)).toBe(true);

  // So is one whose verdict is gone.
  const lost: ConnectionValidation = {
    ...verified,
    throughput: { selection: "auto", state: "stale" },
  };
  expect(roleNeedsValidation(cfg, lost, "throughput", discovery)).toBe(true);
});

// The panel badge reported ready-or-not, so a failed path was indistinguishable
// from a check still running and a refused transport read as permanently
// checking. Each state has to name itself.
test("the panel names a failure rather than reading as a check", () => {
  const present = (
    throughput: ConnectionValidationState,
    latency: ConnectionValidationState,
  ) =>
    ({
      throughput: { validation: throughput },
      latency: { validation: latency },
    }) as unknown as Parameters<typeof panelReadiness>[0];

  expect(panelReadiness(present("verified", "verified"), true)).toBe(
    "verified",
  );
  expect(panelReadiness(present("failed", "verified"), true)).toBe("failed");
  expect(panelReadiness(present("verified", "stale"), true)).toBe("stale");
  // Retrying one role puts it in flight while the other stays failed. The badge
  // is the panel's one summary, so the failure outranks the check: a spinner
  // there says the path may yet come good, and it will not.
  expect(panelReadiness(present("checking", "failed"), true)).toBe("failed");
  expect(panelReadiness(present("failed", "checking"), true)).toBe("failed");
  // Below a failure, a check in flight outranks a role merely owed one: the
  // check is what will resolve it.
  expect(panelReadiness(present("checking", "stale"), true)).toBe("checking");
  // A latency path the run never opens cannot hold the panel back.
  expect(panelReadiness(present("verified", "failed"), false)).toBe("verified");
});
