import { expect, test } from "bun:test";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/endpoints";
import type {
  PreparedPaths,
  RunnerConfig,
  TransportDiscovery,
} from "./contract";
import {
  roleNeedsValidation,
  preparedPaths,
  validationRoles,
  presentConnections,
  panelReadiness,
  CONNECTION_FRESH_MS,
  type ConnectionValidation,
  type ConnectionValidationState,
} from "./connectionModel";
import {
  classifyTransportDiscovery,
  fetchViewOfOrigin,
  selectLatencyTarget,
  selectThroughputTarget,
  ROUTES,
} from "./real/backendPure";
import { testPreparedPaths } from "./test-helpers.test";
import { DEFAULT_CONFIG } from "../state/defaults";
import { estimateCompensation } from "../compensation";

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
function makeDiscovery(
  options: {
    throughput?: Parameters<typeof classifyTransportDiscovery>[0];
    latency?: Parameters<typeof classifyTransportDiscovery>[1];
    pageOrigin?: string;
    pageProtocol?: string;
  } = {},
): TransportDiscovery {
  return {
    ...classifyTransportDiscovery(
      options.throughput ?? [throughput],
      options.latency ?? [latency],
      options.pageOrigin ?? throughput.origin,
      true,
      options.pageProtocol ?? "h2",
    ),
    generation: "generation-a",
    engineVersion: "test",
    server: { name: "meter" },
    fetchedAt: 1,
  };
}
function makePaths(discovery = makeDiscovery()): PreparedPaths {
  const base = testPreparedPaths();
  const transfer = selectThroughputTarget(discovery, "auto", true)!;
  const ping = selectLatencyTarget(discovery, "auto", true);
  return {
    discovery,
    throughput: {
      ...base.throughput,
      requested: structuredClone(transfer),
      target: structuredClone(transfer),
      fetch: structuredClone(
        transfer.transport === "fetch-stream"
          ? transfer
          : fetchViewOfOrigin(discovery, transfer),
      ),
      probe: { ...base.throughput.probe, protocolNegotiated: "h2" },
      browserProtocol: "h2",
      generation: discovery.generation,
    },
    latency: ping
      ? {
          ...base.latency!,
          requested: structuredClone(ping),
          target: structuredClone(ping),
          generation: discovery.generation,
          rttMs: 0,
        }
      : null,
  };
}
function makeValidation(
  paths: PreparedPaths | null = null,
): ConnectionValidation {
  return {
    throughput: {
      selection: "current",
      state: paths ? "verified" : "stale",
      path: paths?.throughput ?? null,
    },
    latency: {
      selection: "auto",
      state: paths?.latency ? "verified" : "stale",
      path: paths?.latency ?? null,
    },
  };
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
  const paths = makePaths();
  paths.throughput.probe = {
    ...paths.throughput.probe,
    clientIpSource: "forwarded",
    protocolNegotiated: "http/1.1",
  };
  paths.latency!.rttMs = 4;
  const validation = makeValidation(paths);
  const model = presentConnections(cfg, paths.discovery, validation);
  expect(model.throughput.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(model.throughput.browserProtocol).toBe("h2");
  expect(model.throughput.serverProtocol).toBe("http/1.1");
  expect(model.throughput.clientIpSource).toBe("forwarded");
  expect(model.throughput.selection).toBe(throughput.origin);
  expect(model.latency.summary).toBe("WebSocket · TLS");
  expect(model.latency.preTestPingMs).toBe(4);
});

test("native H1 latency summary states its deterministic HTTP version", () => {
  const paths = makePaths(
    makeDiscovery({
      throughput: [{ ...throughput, protocol: "http1" }],
      latency: [{ ...latency, origin: throughput.origin }],
      pageProtocol: "http/1.1",
    }),
  );
  expect(
    presentConnections(config(), paths.discovery, makeValidation(paths)).latency
      .summary,
  ).toBe("WebSocket · HTTP/1.1 · TLS");
});

test("negotiated fetch evidence does not rewrite the requested selection", () => {
  const paths = makePaths(
    makeDiscovery({ throughput: [{ ...throughput, protocol: "negotiated" }] }),
  );
  paths.throughput.fetch.protocol = "http2";
  paths.throughput.probe.protocolNegotiated = "http/1.1";
  const validation = makeValidation(paths);
  const model = presentConnections(
    config(),
    paths.discovery,
    validation,
  ).throughput;
  expect(model.summary).toBe("Fetch stream · HTTP/2 · TLS");
  expect(model.observedProtocol).toBe("http2");
  expect(
    roleNeedsValidation(config(), validation, "throughput", paths.discovery),
  ).toBe(false);
  expect(paths.throughput.requested.protocol).toBe("negotiated");
});

test("wire evidence follows WebTransport rather than its HTTP probe", () => {
  withWebTransport(() => {
    const paths = makePaths(
      makeDiscovery({
        throughput: [webTransportOnly],
        latency: [],
        pageOrigin: "https://ui.test",
      }),
    );
    paths.throughput.probe.protocolNegotiated = "http/1.1";
    paths.throughput.fetch.protocol = "http1";
    paths.throughput.browserProtocol = "h3";
    const connection = presentConnections(
      config(),
      paths.discovery,
      makeValidation(paths),
    ).throughput;
    const estimate = estimateCompensation(
      1_000_000,
      connection.browserProtocol,
      connection.target?.tls,
      connection.clientIpVersion,
      connection.target?.transport,
    );
    expect(connection.target?.transport).toBe("webtransport");
    expect(connection.serverProtocol).toBe("http/1.1");
    expect(estimate.transport).toBe("http3-quic");
    expect(estimate.framing).toBe("webtransport-stream");
    expect(estimate.factors.map((factor) => factor.label)).not.toContain(
      "HTTP/3 DATA frames",
    );
  });
});

test.each(["http2", "http3"] as const)(
  "a degraded session presents its actual %s fetch path",
  (protocol) => {
    withWebTransport(() => {
      const paths = makePaths(
        makeDiscovery({
          throughput: [webTransportOnly],
          latency: [],
          pageOrigin: "https://ui.test",
        }),
      );
      paths.throughput.fetch.protocol = protocol;
      paths.throughput.target = paths.throughput.fetch;
      paths.throughput.browserProtocol = protocol === "http2" ? "h2" : "h3";
      const validation = makeValidation(paths);
      const presented = presentConnections(
        config(),
        paths.discovery,
        validation,
      ).throughput;
      expect(presented.target?.transport).toBe("fetch-stream");
      expect(presented.observedProtocol).toBe(protocol);
      expect(presented.summary).toBe(
        `Fetch stream · HTTP/${protocol === "http2" ? "2" : "3"} · TLS`,
      );
      expect(paths.throughput.requested.transport).toBe("webtransport");
      expect(
        roleNeedsValidation(
          config(),
          validation,
          "throughput",
          paths.discovery,
        ),
      ).toBe(false);
    });
  },
);

test("the latency panel names the committed fallback bus", () => {
  withWebTransport(() => {
    const paths = makePaths(
      makeDiscovery({
        latency: [
          { baseUrl: throughput.origin, transport: "websocket" },
          { baseUrl: throughput.origin, transport: "webtransport" },
        ],
      }),
    );
    expect(paths.latency!.requested.transport).toBe("webtransport");
    paths.latency!.target = paths.discovery.latency[
      throughput.origin
    ].targets.find((target) => target.transport === "websocket")!;
    paths.latency!.rttMs = 4;
    const validation = makeValidation(paths);
    const presented = presentConnections(
      config(),
      paths.discovery,
      validation,
    ).latency;
    expect(presented.target?.transport).toBe("websocket");
    expect(presented.summary).toBe("WebSocket · TLS");
    expect(
      roleNeedsValidation(config(), validation, "latency", paths.discovery),
    ).toBe(false);
  });
});

test("zero latency evidence is retained and absent evidence is not fabricated", () => {
  const paths = makePaths();
  delete paths.throughput.browserProtocol;
  const validation = makeValidation(paths);
  const presented = presentConnections(config(), paths.discovery, validation);
  expect(presented.latency.preTestPingMs).toBe(0);
  expect(presented.throughput.browserProtocol).toBeUndefined();
  validation.latency.path!.rttMs = null;
  expect(preparedPaths(config(), paths.discovery, validation)).not.toBeNull();
  expect(
    presentConnections(config(), paths.discovery, validation).latency
      .preTestPingMs,
  ).toBeUndefined();
  validation.latency.path = null;
  expect(
    roleNeedsValidation(config(), validation, "latency", paths.discovery),
  ).toBe(true);
  expect(preparedPaths(config(), paths.discovery, validation)).toBeNull();
  expect(
    presentConnections(config(), paths.discovery, validation).latency
      .preTestPingMs,
  ).toBeUndefined();
});

test("old evidence is hidden after selection, target descriptor, or generation changes", () => {
  const paths = makePaths();
  const validation = makeValidation(paths);
  const cfg = config();
  cfg.transports.throughputTarget = "https://elsewhere.test";
  expect(
    roleNeedsValidation(cfg, validation, "throughput", paths.discovery),
  ).toBe(true);
  expect(
    presentConnections(cfg, paths.discovery, validation).throughput
      .serverProtocol,
  ).toBeUndefined();
  const changed = structuredClone(paths.discovery);
  changed.throughput[throughput.origin].targets[0].routes.probe =
    "/different-probe";
  expect(roleNeedsValidation(config(), validation, "throughput", changed)).toBe(
    true,
  );
  expect(roleNeedsValidation(config(), validation, "latency", changed)).toBe(
    false,
  );
  changed.generation = "generation-b";
  for (const role of ["throughput", "latency"] as const)
    expect(roleNeedsValidation(config(), validation, role, changed)).toBe(true);
  const model = presentConnections(config(), changed, validation);
  expect(model.throughput.serverProtocol).toBeUndefined();
  expect(model.latency.preTestPingMs).toBeUndefined();
  expect(preparedPaths(config(), changed, validation)).toBeNull();
});

test("a frozen run keeps its committed paths when draft validation changes", () => {
  const paths = makePaths();
  const cfg = config();
  cfg.transports.throughputTarget = "https://elsewhere.test";
  const next = { ...paths.discovery, generation: "next" };
  const model = presentConnections(cfg, next, makeValidation(), paths);
  expect(model.throughput.target).toEqual(paths.throughput.target);
  expect(model.throughput.serverProtocol).toBe(
    paths.throughput.probe.protocolNegotiated,
  );
  expect(model.throughput.validation).toBe("verified");
  expect(model.latency.preTestPingMs).toBe(0);
});

test("equivalent selections and display/stage edits reuse verified paths", () => {
  const paths = makePaths();
  const validation = makeValidation(paths);
  const cfg = config();
  cfg.transports.throughputTarget = throughput.origin;
  cfg.transports.latencyTarget = latency.origin;
  cfg.visualization.throughputMaxBytesPerSec = 1_000_000;
  cfg.duration.downloadMs += 1_000;
  cfg.stages.download = false;
  expect(
    roleNeedsValidation(cfg, validation, "throughput", paths.discovery),
  ).toBe(false);
  expect(roleNeedsValidation(cfg, validation, "latency", paths.discovery)).toBe(
    false,
  );
  const prepared = preparedPaths(cfg, paths.discovery, validation);
  expect(prepared?.throughput).toBe(paths.throughput);
  expect(prepared?.latency).toBe(paths.latency);
  cfg.transports.latencyTarget = "https://elsewhere.test";
  expect(roleNeedsValidation(cfg, validation, "latency", paths.discovery)).toBe(
    true,
  );
  expect(
    roleNeedsValidation(cfg, validation, "throughput", paths.discovery),
  ).toBe(false);
});

test("prepared runs require fresh evidence for every needed role", () => {
  const paths = makePaths();
  const validation = makeValidation(paths);
  expect(preparedPaths(config(), paths.discovery, validation)).not.toBeNull();
  for (const role of ["throughput", "latency"] as const) {
    const path = validation[role].path!;
    const verifiedAt = path.verifiedAt;
    path.verifiedAt = Date.now() - CONNECTION_FRESH_MS - 1_000;
    expect(preparedPaths(config(), paths.discovery, validation)).toBeNull();
    path.verifiedAt = verifiedAt;
  }
  for (const state of ["stale", "checking", "failed"] as const) {
    validation.throughput.state = state;
    expect(preparedPaths(config(), paths.discovery, validation)).toBeNull();
  }
  validation.throughput.state = "verified";
  validation.throughput.path = null;
  expect(preparedPaths(config(), paths.discovery, validation)).toBeNull();
  expect(preparedPaths(config(), null, makeValidation(paths))).toBeNull();
});

test("loaded latency determines whether missing latency blocks preparation", () => {
  const paths = makePaths();
  const validation = makeValidation(paths);
  validation.latency = { selection: "auto", state: "stale", path: null };
  const cfg = config();
  cfg.stages.latency = false;
  cfg.skipLoadedLatencyWhenStageOff = true;
  expect(roleNeedsValidation(cfg, validation, "latency", paths.discovery)).toBe(
    false,
  );
  expect(preparedPaths(cfg, paths.discovery, validation)?.latency).toBeNull();
  cfg.skipLoadedLatencyWhenStageOff = false;
  expect(roleNeedsValidation(cfg, validation, "latency", paths.discovery)).toBe(
    true,
  );
  expect(preparedPaths(cfg, paths.discovery, validation)).toBeNull();
});

test("validation retries the changed role and carries an aborted stale role", () => {
  const paths = makePaths();
  const validation = makeValidation(paths);
  const cfg = config();
  cfg.transports.throughputTarget = "https://elsewhere.test";
  expect(
    validationRoles(cfg, validation, "throughput", paths.discovery),
  ).toEqual(["throughput"]);
  expect(validationRoles(cfg, validation, "latency", paths.discovery)).toEqual([
    "throughput",
    "latency",
  ]);
  cfg.transports.throughputTarget = throughput.origin;
  validation.throughput = {
    selection: throughput.origin,
    state: "stale",
    path: null,
  };
  expect(validationRoles(cfg, validation, "latency", paths.discovery)).toEqual([
    "throughput",
    "latency",
  ]);
  cfg.stages.latency = false;
  cfg.skipLoadedLatencyWhenStageOff = true;
  expect(
    validationRoles(cfg, validation, "throughput", paths.discovery),
  ).toEqual(["throughput"]);
});

test("failed probes remain retryable without displaying stale evidence", () => {
  const paths = makePaths();
  const validation = makeValidation(paths);
  validation.throughput.state = "failed";
  validation.throughput.message = "probe timed out";
  const model = presentConnections(config(), paths.discovery, validation);
  expect(model.throughput.availability).toBe("advertised");
  expect(model.throughput.validation).toBe("failed");
  expect(model.throughput.message).toBe("probe timed out");
  expect(model.throughput.serverProtocol).toBeUndefined();
});

test("an explicit session shares its origin availability but still needs browser support", () => {
  const cfg = config();
  cfg.transports.throughputTarget = `${throughput.origin}::wt`;
  const discovery = makeDiscovery({
    throughput: [
      throughput,
      { ...webTransportOnly, baseUrl: throughput.origin },
    ],
  });
  const validation = makeValidation();
  validation.throughput.state = "checking";
  const model = presentConnections(cfg, discovery, validation);
  expect(model.throughput.availability).toBe("advertised");
  expect(model.throughput.target).toBeNull();
  withWebTransport(() => {
    expect(
      presentConnections(cfg, discovery, validation).throughput.target
        ?.transport,
    ).toBe("webtransport");
  });
  cfg.transports.throughputTarget = "auto";
  const unsupported = presentConnections(
    cfg,
    makeDiscovery({
      throughput: [webTransportOnly],
      latency: [],
      pageOrigin: "https://ui.test",
    }),
    validation,
  ).throughput;
  expect(unsupported.target).toBeNull();
  expect(unsupported.availability).toBe("not-advertised");
  expect(unsupported.summary).toBe("Selection unresolved");
});

test("the panel names a failure rather than reading as a check", () => {
  const model = (
    throughput: ConnectionValidationState,
    latency: ConnectionValidationState,
  ) =>
    ({
      throughput: { validation: throughput },
      latency: { validation: latency },
    }) as Parameters<typeof panelReadiness>[0];
  expect(panelReadiness(model("verified", "verified"), true)).toBe("verified");
  expect(panelReadiness(model("failed", "verified"), true)).toBe("failed");
  expect(panelReadiness(model("verified", "stale"), true)).toBe("stale");
  expect(panelReadiness(model("checking", "failed"), true)).toBe("failed");
  expect(panelReadiness(model("failed", "checking"), true)).toBe("failed");
  expect(panelReadiness(model("checking", "stale"), true)).toBe("checking");
  expect(panelReadiness(model("verified", "failed"), false)).toBe("verified");
});

test("policy changes retain verified roles and check only newly needed latency", () => {
  const cfg = config();
  const paths = makePaths();
  const validation = makeValidation(paths);
  expect(validationRoles(cfg, validation, undefined, paths.discovery)).toEqual(
    [],
  );
  cfg.stages.latency = false;
  cfg.skipLoadedLatencyWhenStageOff = true;
  validation.latency = { selection: "auto", state: "stale", path: null };
  expect(validationRoles(cfg, validation, undefined, paths.discovery)).toEqual(
    [],
  );
  cfg.stages.latency = true;
  expect(validationRoles(cfg, validation, undefined, paths.discovery)).toEqual([
    "latency",
  ]);
  expect(validation.throughput.path).toBe(paths.throughput);
});

test("expiry is an on-demand role check without changing cached timestamps", () => {
  const cfg = config();
  const paths = makePaths();
  const validation = makeValidation(paths);
  const verifiedAt = Date.now() - CONNECTION_FRESH_MS - 1000;
  validation.latency.path!.verifiedAt = verifiedAt;
  expect(validationRoles(cfg, validation, undefined, paths.discovery)).toEqual([
    "latency",
  ]);
  expect(
    validationRoles(cfg, validation, undefined, paths.discovery, Infinity),
  ).toEqual([]);
  expect(
    preparedPaths(cfg, paths.discovery, validation, Infinity)?.latency
      ?.verifiedAt,
  ).toBe(verifiedAt);
  expect(
    validationRoles(cfg, validation, "throughput", paths.discovery, Infinity),
  ).toEqual(["throughput"]);
  expect(validationRoles(cfg, validation, "all", paths.discovery)).toEqual([
    "throughput",
    "latency",
  ]);
});

test("a manual role retry leaves an unrelated failed role available for its own retry", () => {
  const cfg = config();
  const paths = makePaths();
  const validation = makeValidation(paths);
  validation.latency = {
    selection: cfg.transports.latencyTarget,
    state: "failed",
    path: null,
  };
  expect(
    validationRoles(cfg, validation, "throughput", paths.discovery),
  ).toEqual(["throughput"]);
  expect(validationRoles(cfg, validation, undefined, paths.discovery)).toEqual([
    "latency",
  ]);
});
