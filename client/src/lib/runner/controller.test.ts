// Contracts of the connection model, its owner, and the application controller that drives them.
import "../state/runes.testutil";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  expect,
  spyOn,
  test,
} from "bun:test";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/endpoints";
import type {
  NetworkRunner,
  PreparedPaths,
  RunnerConfig,
  RunnerEvent,
  TransportDiscovery,
} from "./contract";
import {
  summarizeRoleValidation,
  uploadCapabilityFailure,
  connectionDraftRoleKey,
  roleNeedsValidation,
  preparedPaths,
  presentConnections,
  CONNECTION_FRESH_MS,
  emptyConnectionValidation,
  type ConnectionValidation,
} from "./connectionModel";
import {
  classifyTransportDiscovery,
  fetchViewOfOrigin,
  selectLatencyTarget,
  selectThroughputTarget,
  ROUTES,
} from "./real/backendPure";
import type { ConnectionPreparation } from "./real/prepare";
import {
  PreflightUnavailableError,
  TransportUnavailableError,
} from "./real/transportError";
import {
  ServerConnections,
  type ServerConnectionView,
} from "../servers/connections";
import { ServerAuthenticationRequired } from "../servers/credentials";
import { DEFAULT_CONFIG } from "../state/defaults";
import { stubGlobals } from "../test-helpers.testutil";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
  testServerCatalog,
  testServerDiscovery,
} from "./test-helpers.testutil";

// Store and runner modules read build tokens when they first load.
let restoreBuild: () => void;
beforeAll(() => (restoreBuild = stubGlobals(TEST_BUILD_TOKENS)));
afterAll(() => restoreBuild());

/* ---------- Pure connection model ---------- */
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
    uploadCheckpoint: true,
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
      selection: "auto",
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

test("participant role summaries never borrow another role's failure or checking state", () => {
  const paths = makePaths();
  const a = makeValidation(paths);
  const b = makeValidation(paths);
  b.throughput = { selection: "auto", state: "failed", path: null };
  const discoveries = new Map([
    ["a", paths.discovery],
    ["b", paths.discovery],
  ]);
  const validations = new Map([
    ["a", a],
    ["b", b],
  ]);
  const summary = (role: "throughput" | "latency") =>
    summarizeRoleValidation(
      config(),
      role,
      ["a", "b"],
      discoveries,
      validations,
    );
  expect(summary("throughput")).toEqual({
    state: "failed",
    verified: 1,
    total: 2,
  });
  expect(summary("latency")).toEqual({
    state: "verified",
    verified: 2,
    total: 2,
  });
  b.throughput.state = "checking";
  expect(summary("throughput").state).toBe("checking");
  expect(summary("latency").state).toBe("verified");
  discoveries.set("b", { ...paths.discovery, generation: "new" });
  expect(summary("latency")).toEqual({ state: "stale", verified: 1, total: 2 });
  expect(
    summarizeRoleValidation(
      config(),
      "latency",
      ["a", "missing"],
      discoveries,
      validations,
    ),
  ).toEqual({ state: "stale", verified: 1, total: 2 });
});

test("upload capability blocks prepared paths and throughput presentation without erasing probe evidence", () => {
  const cfg = config();
  cfg.stages = {
    latency: true,
    download: true,
    upload: false,
    bidirectional: false,
  };
  const paths = makePaths();
  paths.discovery.uploadCheckpoint = false;
  const validation = makeValidation(paths);
  const initialKey = connectionDraftRoleKey(cfg, "throughput");
  expect(preparedPaths(cfg, paths.discovery, validation)).not.toBeNull();
  cfg.stages.upload = true;
  expect(connectionDraftRoleKey(cfg, "throughput")).not.toBe(initialKey);
  expect(uploadCapabilityFailure(cfg, paths.discovery)).toContain("checkpoint");
  expect(preparedPaths(cfg, paths.discovery, validation)).toBeNull();
  expect(
    roleNeedsValidation(cfg, validation, "throughput", paths.discovery),
  ).toBe(false);
  const view = presentConnections(cfg, paths.discovery, validation);
  expect(view.throughput.validation).toBe("failed");
  expect(view.throughput.message).toBe(
    uploadCapabilityFailure(cfg, paths.discovery),
  );
  expect(view.latency.validation).toBe("verified");
  const discoveries = new Map([["self", paths.discovery]]);
  const validations = new Map([["self", validation]]);
  expect(
    summarizeRoleValidation(
      cfg,
      "throughput",
      ["self"],
      discoveries,
      validations,
    ),
  ).toEqual({ state: "failed", verified: 0, total: 1 });
  expect(
    summarizeRoleValidation(cfg, "latency", ["self"], discoveries, validations)
      .state,
  ).toBe("verified");
  expect(validation.throughput.state).toBe("verified");
  cfg.stages.upload = false;
  expect(connectionDraftRoleKey(cfg, "throughput")).toBe(initialKey);
  expect(preparedPaths(cfg, paths.discovery, validation)).not.toBeNull();
  expect(
    presentConnections(cfg, paths.discovery, validation).throughput.validation,
  ).toBe("verified");
  cfg.stages.bidirectional = true;
  expect(preparedPaths(cfg, paths.discovery, validation)).toBeNull();
  paths.discovery.uploadCheckpoint = true;
  expect(preparedPaths(cfg, paths.discovery, validation)).not.toBeNull();
});

/* ---------- Connection owner ---------- */
let restore: () => void;
beforeEach(() => {
  restore = stubGlobals({ location: new URL("http://meter.test") });
});
afterEach(() => restore());

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
async function settle() {
  for (let i = 0; i < 40; i++) await Promise.resolve();
}
function preparation(): ConnectionPreparation {
  const paths = testPreparedPaths();
  return {
    discovery: paths.discovery,
    validation: {
      throughput: {
        selection: "auto",
        state: "verified",
        path: paths.throughput,
      },
      latency: { selection: "auto", state: "verified", path: paths.latency },
    },
  };
}
function fixture(
  prepare: ConstructorParameters<typeof ServerConnections>[0]["prepare"],
  discover: ConstructorParameters<
    typeof ServerConnections
  >[0]["discover"] = async () => testPreparedPaths().discovery,
) {
  const views = new Map<string, ServerConnectionView>();
  const manager = new ServerConnections({
    discover,
    prepare,
    changed: (changed) => {
      for (const view of changed) views.set(view.server.id, view);
    },
    idleEvent() {},
  });
  manager.reset(
    ["self", "peer"].map((id) => ({ id, name: id, url: "http://meter.test" })),
  );
  const config = structuredClone(DEFAULT_CONFIG);
  manager.select([
    { id: "self", config },
    { id: "peer", config },
  ]);
  return { manager, views, config };
}

test("a failed role preserves the independently verified role", async () => {
  const { manager, views } = fixture(async (_config, _previous, roles) => {
    if (roles.includes("latency")) throw new Error("offline");
    return preparation();
  });
  try {
    await expect(manager.check({ ids: ["self"] })).rejects.toThrow();
    expect(views.get("self")!.validation.throughput.state).toBe("verified");
    expect(views.get("self")!.validation.latency.state).toBe("failed");
    expect(manager.paths("self")).toBeNull();
  } finally {
    manager.dispose();
  }
});

test("wrapped remote authentication retains actionable sign-in details", async () => {
  const { manager, views } = fixture(
    async () => preparation(),
    async (_signal, context) => {
      throw new PreflightUnavailableError("preflight unavailable", {
        cause: new ServerAuthenticationRequired(context!.server),
      });
    },
  );
  try {
    await expect(manager.check({ ids: ["peer"] })).rejects.toThrow(
      "Sign in to peer",
    );
    expect(views.get("peer")!.readiness).toMatchObject({
      state: "sign-in",
      message: "Sign in to peer",
    });
  } finally {
    manager.dispose();
  }
});

test("fresh equivalent selections reuse evidence, but expired reselections refresh discovery and both roles", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  const checked: string[] = [];
  const refreshed = deferred<void>();
  let discoveries = 0;
  const { manager, views, config } = fixture(
    async (_config, _previous, roles) => {
      checked.push(...roles);
      if (checked.length === 4) refreshed.resolve();
      return preparation();
    },
    async () => {
      discoveries++;
      return testPreparedPaths().discovery;
    },
  );
  try {
    manager.select([{ id: "self", config }]);
    await manager.check();
    const equivalent = structuredClone(config);
    equivalent.transports.throughputTarget =
      preparation().validation.throughput.path!.target.origin;
    equivalent.transports.latencyTarget =
      preparation().validation.latency.path!.target.origin;
    manager.select([{ id: "self", config: equivalent }]);
    await manager.check();
    expect(checked).toEqual(["throughput", "latency"]);
    expect(manager.ready()).toBe(true);
    manager.select([]);
    clock.mockReturnValue(1000 + CONNECTION_FRESH_MS + 1);
    manager.select([{ id: "self", config: equivalent }]);
    expect(manager.ready()).toBe(false);
    expect(views.get("self")!.readiness.state).toBe("unchecked");
    manager.activity(true, null);
    await refreshed.promise;
    await settle();
    expect(discoveries).toBe(2);
    expect(checked).toEqual(["throughput", "latency", "throughput", "latency"]);
    expect(manager.ready()).toBe(true);
  } finally {
    manager.dispose();
    clock.mockRestore();
  }
});

test("a generation change cancels an in-flight role before accepting replacement evidence", async () => {
  const held = deferred<ConnectionPreparation>();
  let generation = "gen-a";
  let block = false;
  let oldSignal: AbortSignal | undefined;
  const { manager, views } = fixture(
    async (_config, _previous, roles, signal) => {
      if (block && roles.includes("latency")) {
        oldSignal = signal;
        return held.promise;
      }
      const result = preparation();
      result.validation.throughput.path!.generation = generation;
      result.validation.latency.path!.generation = generation;
      return result;
    },
    async () => ({ ...testPreparedPaths().discovery, generation }),
  );
  try {
    await manager.check({ ids: ["self"] });
    block = true;
    const old = manager
      .check({ ids: ["self"], role: "latency", force: true })
      .catch((error) => error);
    await settle();
    expect(oldSignal?.aborted).toBe(false);
    generation = "gen-b";
    block = false;
    await manager.check({ ids: ["self"], role: "throughput", force: true });
    expect((await old).name).toBe("AbortError");
    expect(oldSignal?.aborted).toBe(true);
    expect(manager.ready(["self"])).toBe(true);
    held.resolve(preparation());
    await settle();
    expect(views.get("self")!.validation.latency.path!.generation).toBe(
      "gen-b",
    );
  } finally {
    manager.dispose();
  }
});

test("changing the latency participant cancels its probe and cannot adopt late evidence when re-enabled", async () => {
  const held = deferred<ConnectionPreparation>();
  let latencyChecks = 0;
  let stopped = 0;
  const { manager, views, config } = fixture(
    async (_config, _previous, roles) => {
      if (roles.includes("latency") && ++latencyChecks === 1)
        return held.promise;
      return preparation();
    },
  );
  try {
    manager.select([{ id: "self", config }]);
    const old = manager.check().catch((error) => error);
    await settle();
    expect(views.get("self")!.validation.throughput.state).toBe("verified");
    const noLatency = structuredClone(config);
    noLatency.stages.latency = false;
    noLatency.skipLoadedLatencyWhenStageOff = true;
    manager.select([{ id: "self", config: noLatency }]);
    expect((await old).name).toBe("AbortError");
    expect(manager.ready()).toBe(true);
    manager.select([{ id: "self", config }]);
    expect(manager.ready()).toBe(false);
    await manager.check();
    const current = views.get("self")!.validation.latency.path;
    held.resolve({
      ...preparation(),
      idle: {
        start() {},
        stop() {
          stopped++;
        },
        onEvent() {},
      },
    });
    await settle();
    expect(latencyChecks).toBe(2);
    expect(stopped).toBe(1);
    expect(views.get("self")!.validation.latency.path).toBe(current);
    expect(manager.ready()).toBe(true);
  } finally {
    manager.dispose();
  }
});

test("expired grants cannot reuse fresh prepared paths on immediate reselection", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  const { manager, config, views } = fixture(async () => preparation());
  try {
    manager.select([{ id: "peer", config }]);
    manager.authorize({
      ...manager.credentials("peer")!,
      kind: "grant",
      token: "test-only",
      expiresAt: 2000,
    });
    await manager.check();
    expect(manager.ready()).toBe(true);
    manager.select([]);
    clock.mockReturnValue(2001);
    manager.select([{ id: "peer", config }]);
    expect(manager.ready()).toBe(false);
    expect(views.get("peer")!.readiness).toMatchObject({
      state: "sign-in",
      message: "Sign in to node-a",
    });
  } finally {
    manager.dispose();
    clock.mockRestore();
  }
});

test("cancelling Start aborts its probes without blocking a later check", async () => {
  const held = deferred<ConnectionPreparation>();
  const signals: AbortSignal[] = [];
  let block = true;
  const { manager } = fixture(async (_config, _previous, _roles, signal) => {
    signals.push(signal!);
    return block ? held.promise : preparation();
  });
  try {
    const start = new AbortController();
    const pending = manager
      .check({ ids: ["self"], signal: start.signal })
      .catch((error) => error);
    await settle();
    expect(signals.length).toBe(2);
    start.abort();
    expect((await pending).name).toBe("AbortError");
    expect(signals.every((signal) => signal.aborted)).toBe(true);
    block = false;
    await settle();
    await manager.check({ ids: ["self"] });
    expect(manager.ready(["self"])).toBe(true);
    held.resolve(preparation());
  } finally {
    manager.dispose();
  }
});

test("renewed authorization discards old work and resumes only the affected server", async () => {
  const held = deferred<ConnectionPreparation>();
  let block = true;
  const checked: string[] = [];
  const { manager, views } = fixture(
    async (_config, _previous, _roles, _signal, credentials) => {
      checked.push(credentials!.server.id);
      return block ? held.promise : preparation();
    },
  );
  try {
    const old = manager.check({ ids: ["self"] }).catch((error) => error);
    await settle();
    manager.requireAuthentication("self", "Sign in again");
    expect(views.get("self")!.readiness.state).toBe("sign-in");
    expect((await old).name).toBe("AbortError");
    const credentials = manager.credentials("self")!;
    manager.authorize({
      ...credentials,
      kind: "grant",
      token: "test-only",
      expiresAt: Date.now() + 60000,
    });
    block = false;
    await manager.check({ ids: ["self"] });
    expect(manager.ready(["self"])).toBe(true);
    expect(checked.every((id) => id === "self")).toBe(true);
    held.resolve(preparation());
    await settle();
    expect(views.get("self")!.readiness.state).toBe("ready");
  } finally {
    manager.dispose();
  }
});

test("slow background discovery leaves capacity for a newly selected server", async () => {
  const background = deferred<void>();
  const started: string[] = [];
  const { manager, config } = fixture(
    async () => preparation(),
    async (_signal, credentials) => {
      started.push(credentials!.server.id);
      if (credentials!.server.id !== "selected") await background.promise;
      return testPreparedPaths().discovery;
    },
  );
  try {
    manager.reset(
      ["slow-a", "slow-b", "selected"].map((id) => ({
        id,
        name: id,
        url: "http://meter.test",
      })),
    );
    manager.metadata(true);
    manager.activity(true, null);
    for (let i = 0; i < 100 && !started.length; i++) await Bun.sleep(5);
    expect(started).toEqual(["slow-a"]);
    manager.select([{ id: "selected", config }]);
    await manager.check();
    expect(manager.paths("selected")).not.toBeNull();
    expect(started).toEqual(["slow-a", "selected"]);
  } finally {
    background.resolve();
    manager.dispose();
    await settle();
  }
});

test("operations publish changed views once, without reentrant churn", async () => {
  const batches: string[][] = [];
  const manager: ServerConnections = new ServerConnections({
    discover: async () => testPreparedPaths().discovery,
    prepare: async () => preparation(),
    changed: (views) => {
      batches.push(views.map((view) => view.server.id));
      // A consumer reacting to a publication may call back into the owner.
      manager.activity(true, null);
    },
    idleEvent() {},
  });
  manager.reset(
    ["self", "peer"].map((id) => ({ id, name: id, url: "http://meter.test" })),
  );
  expect(batches).toEqual([["self", "peer"]]);
  const config = structuredClone(DEFAULT_CONFIG);
  const selection = [
    { id: "self", config },
    { id: "peer", config },
  ];
  manager.select(selection);
  await manager.check();
  await settle();
  const published = batches.length;
  manager.select(selection);
  manager.activity(true, null);
  expect(batches).toHaveLength(published);
  expect(manager.ready()).toBe(true);
  manager.dispose();
});

/* ---------- Application controller ---------- */
function stubEngineGlobals(): () => void {
  return stubGlobals({ ...TEST_BUILD_TOKENS, window: undefined });
}
async function yieldUntil(done: () => boolean, turns = 10): Promise<void> {
  for (let turn = 0; turn < turns && !done(); turn++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}
async function withBootRunner(
  run: (
    engine: import("./engine.svelte").ApplicationController,
  ) => Promise<void>,
  setup: () => () => void = () => stubBootEnvironment("visible"),
): Promise<void> {
  const restoreGlobals = stubEngineGlobals();
  const restoreEnvironment = setup();
  const { createApplicationController } = await import("./engine.svelte");
  const { store } = await import("../state/store.svelte");
  const { prepareConnections } = await import("./real/prepare");
  const engine = createApplicationController(store, {
    loadCatalog: testServerCatalog,
    prepare: prepareConnections,
  });
  try {
    await engine.boot();
    await run(engine);
  } finally {
    engine.dispose();
    restoreEnvironment();
    restoreGlobals();
  }
}
function stubGlobal(key: string, value: unknown): () => void {
  return stubGlobals({ [key]: value });
}
function stubBootEnvironment(visibility: "hidden" | "visible"): () => void {
  const origin = new URL("https://meter.test/");
  const restores = [
    stubGlobal("location", origin),
    stubGlobal("window", {
      location: origin,
      addEventListener() {},
      removeEventListener() {},
    }),
    stubGlobal("document", {
      visibilityState: visibility,
      querySelector: () => null,
      addEventListener() {},
      removeEventListener() {},
    }),
    stubGlobal("fetch", () => Promise.reject(new Error("no network"))),
  ];
  return () => {
    for (const restore of restores.reverse()) restore();
  };
}
function eventTarget() {
  const listeners = new Map<string, (event: Event) => void>();
  return {
    addEventListener(type: string, listener: (event: Event) => void) {
      listeners.set(type, listener);
    },
    removeEventListener(type: string) {
      listeners.delete(type);
    },
    emit(type: string) {
      listeners.get(type)?.(new Event(type));
    },
  };
}
function stubEventBootEnvironment(
  visibility: "hidden" | "visible",
  online: boolean,
) {
  const windowListeners = eventTarget();
  const documentListeners = eventTarget();
  const documentState = {
    visibilityState: visibility,
    querySelector: () => null,
    addEventListener: documentListeners.addEventListener,
    removeEventListener: documentListeners.removeEventListener,
  };
  const windowValue = {
    addEventListener: windowListeners.addEventListener,
    removeEventListener: windowListeners.removeEventListener,
  };
  const restores = [
    stubGlobal("location", new URL("http://meter.test/")),
    stubGlobal("window", windowValue),
    stubGlobal("document", documentState),
    stubGlobal("navigator", { onLine: online }),
  ];
  return {
    emit(type: string) {
      windowListeners.emit(type);
    },
    setVisibility(next: "hidden" | "visible") {
      documentState.visibilityState = next;
      documentListeners.emit("visibilitychange");
    },
    restore() {
      for (const restore of restores.reverse()) restore();
    },
  };
}
async function settleValidation(): Promise<void> {
  await yieldUntil(() => false);
}
function stubValidationTimers() {
  const realNow = Date.now;
  const realSetTimeout = globalThis.setTimeout;
  const realClearTimeout = globalThis.clearTimeout;
  let now = realNow();
  let nextId = 1;
  const timers = new Map<number, { at: number; run: () => void }>();
  Date.now = () => now;
  globalThis.setTimeout = ((run: () => void, delay = 0) => {
    const id = nextId++;
    timers.set(id, { at: now + delay, run });
    return id as unknown as ReturnType<typeof setTimeout>;
  }) as typeof setTimeout;
  globalThis.clearTimeout = ((id: ReturnType<typeof setTimeout>) => {
    timers.delete(id as unknown as number);
  }) as typeof clearTimeout;
  return {
    delays: () => [...timers.values()].map(({ at }) => at - now),
    advance(milliseconds: number) {
      now += milliseconds;
      for (;;) {
        const due = [...timers.entries()]
          .filter(([, timer]) => timer.at <= now)
          .sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) return;
        timers.delete(due[0]);
        due[1].run();
      }
    },
    size: () => timers.size,
    restore() {
      Date.now = realNow;
      globalThis.setTimeout = realSetTimeout;
      globalThis.clearTimeout = realClearTimeout;
    },
  };
}
async function settleMicrotasks(): Promise<void> {
  for (let turn = 0; turn < 10; turn++) await Promise.resolve();
}
class TestRunner implements NetworkRunner {
  phase: NetworkRunner["phase"] = "idle";
  listener: (event: RunnerEvent) => void = () => {};
  starts = 0;
  start() {
    this.starts++;
    this.phase = "download";
    this.listener({
      type: "phase",
      transition: { from: "idle", to: "download", stage: "download", t: 0 },
    });
  }
  abort() {
    const from = this.phase;
    this.phase = "aborted";
    this.listener({
      type: "phase",
      transition: { from, to: "aborted", stage: null, t: 0 },
    });
  }
  dispose() {}
  reconfigure() {}
  on(listener: (event: RunnerEvent) => void) {
    this.listener = listener;
    return () => {
      this.listener = () => {};
    };
  }
}
type ValidationContext = {
  engine: import("./engine.svelte").ApplicationController;
  runner: TestRunner;
  emit: (event: RunnerEvent) => void;
  environment: ReturnType<typeof stubEventBootEnvironment>;
  probeCalls: () => number;
  idleStops: () => number;
  idleActive: () => boolean;
};
async function withValidationRunner(
  probe: (serverId?: string) => Promise<PreparedPaths>,
  run: (context: ValidationContext) => Promise<void>,
  adoptionState: () => "connected" | "offline" | undefined = () => undefined,
  loadCatalog = testServerCatalog,
  discover = testServerDiscovery,
): Promise<void> {
  const restoreGlobals = stubEngineGlobals();
  const environment = stubEventBootEnvironment("visible", true);
  const { createApplicationController } = await import("./engine.svelte");
  const { store } = await import("../state/store.svelte");
  let calls = 0;
  let stops = 0;
  let idleActive = false;
  let onEvent: (event: RunnerEvent) => void = () => {};
  const runner = new TestRunner();
  const engine = createApplicationController(store, {
    loadCatalog,
    discover,
    createRunner: () => runner,
    prepare: async (config, previous, roles, _signal, credentials) => {
      calls++;
      try {
        const paths = await probe(credentials?.server.id);
        paths.throughput.verifiedAt = Date.now();
        if (paths.latency) paths.latency.verifiedAt = Date.now();
        return {
          discovery: paths.discovery,
          validation: {
            throughput: {
              selection: config.transports.throughputTarget,
              state: "verified",
              path: paths.throughput,
            },
            latency: {
              selection: config.transports.latencyTarget,
              state: "verified",
              path: paths.latency,
            },
          },
          idle: roles.includes("latency")
            ? {
                start() {
                  idleActive = true;
                },
                stop() {
                  idleActive = false;
                  stops++;
                },
                get onEvent() {
                  return onEvent;
                },
                set onEvent(value) {
                  onEvent = value;
                  const state = adoptionState();
                  if (state) value({ type: "connectivity", state });
                },
              }
            : undefined,
        };
      } catch (cause) {
        if (!(cause instanceof TransportUnavailableError) || !cause.role)
          throw cause;
        return {
          discovery: PROBE_EVIDENCE.discovery,
          validation: {
            ...previous,
            [cause.role]: {
              selection:
                config.transports[
                  cause.role === "throughput"
                    ? "throughputTarget"
                    : "latencyTarget"
                ],
              state: "failed",
              path: null,
            },
          },
          failure: cause,
        };
      }
    },
  });
  try {
    await engine.boot();
    await run({
      engine,
      runner,
      emit: (event) => onEvent(event),
      environment,
      probeCalls: () => calls,
      idleStops: () => stops,
      idleActive: () => idleActive,
    });
  } finally {
    engine.dispose();
    environment.restore();
    restoreGlobals();
  }
}
const PROBE_EVIDENCE = testPreparedPaths();

test("switching servers carries transport preferences and clears the old server's paths immediately", async () => {
  const { store } = await import("../state/store.svelte");
  const previous = JSON.parse(JSON.stringify(store.config));
  try {
    await withValidationRunner(
      async () => testPreparedPaths(),
      async ({ engine }) => {
        const paths = testPreparedPaths();
        engine.selectConnection("throughput", paths.throughput.target.id);
        engine.selectConnection("latency", paths.latency!.target.id);
        expect(engine.applyServers(["peer"])).toBe(true);
        expect(store.config.transports).toEqual({
          throughputTarget: "protocol:http1",
          latencyTarget: "transport:websocket",
        });
        expect(store.transportDiscovery).toBeNull();
        expect(store.connectionValidation.throughput.path).toBeNull();
        expect(store.connectionValidation.latency.path).toBeNull();
      },
      undefined,
      async () => {
        const catalog = await testServerCatalog();
        catalog.servers.push({
          id: "peer",
          name: "Peer",
          url: "https://peer.test",
        });
        return catalog;
      },
    );
  } finally {
    store.config = previous;
  }
});

test("superseding start validation cannot leave the application stuck preparing", async () => {
  let calls = 0;
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await withValidationRunner(
    async () => {
      if (++calls === 3) await held;
      return testPreparedPaths();
    },
    async ({ engine, runner }) => {
      const { store } = await import("../state/store.svelte");
      store.serverValidation.get("self")!.throughput.path!.verifiedAt =
        Date.now() - CONNECTION_FRESH_MS - 1;
      engine.toggleRun();
      await yieldUntil(() => calls === 3);
      expect(store.preparing).toBe(true);
      await engine.validateConnections(true);
      release();
      await yieldUntil(() => !engine.hasPendingStart());
      expect(store.preparing).toBe(false);
      expect(store.preparationStatus).toBe("idle");
      expect(runner.starts).toBe(0);
      engine.toggleRun();
      await yieldUntil(() => runner.starts > 0);
      expect(runner.starts).toBe(1);
    },
  );
});

test("an explicit second start click cancels a pending preflight", async () => {
  await withBootRunner(async ({ hasPendingStart, toggleRun }) => {
    const { store } = await import("../state/store.svelte");
    let pendingSignal: AbortSignal | undefined;
    const restorePendingFetch = stubGlobal(
      "fetch",
      (_input: RequestInfo | URL, init?: RequestInit) => {
        pendingSignal = init?.signal ?? undefined;
        return new Promise<Response>(() => {});
      },
    );
    store.reset();
    toggleRun();
    expect(hasPendingStart()).toBe(true);
    expect(store.preparing).toBe(true);
    await yieldUntil(() => pendingSignal !== undefined);
    toggleRun();
    expect(hasPendingStart()).toBe(false);
    expect(store.preparing).toBe(false);
    expect(pendingSignal?.aborted).toBe(true);
    expect(store.phase).toBe("idle");
    expect(store.startError).toBe("");
    expect(store.preparation.status).toBe("idle");
    restorePendingFetch();
  });
});

test("a preflight failure stays idle instead of manufacturing a run error", async () => {
  await withBootRunner(async ({ toggleRun }) => {
    const { store } = await import("../state/store.svelte");
    const restoreFetch = stubGlobal("fetch", () =>
      Promise.reject(new Error("offline")),
    );
    store.reset();
    toggleRun();
    await yieldUntil(() => !store.preparing);
    expect(store.phase).toBe("idle");
    expect(store.startError).toBe("Connection check failed");
    expect(store.preparation.status).toBe("failed");
    restoreFetch();
  });
});

test("visibility resume reuses fresh checks and refreshes expired discovery after a server restart", async () => {
  const timers = stubValidationTimers();
  let generation = "gen-a";
  let discoveries = 0;
  const evidence = () => {
    const paths = testPreparedPaths();
    paths.discovery.generation = generation;
    paths.throughput.generation = generation;
    paths.latency!.generation = generation;
    return paths;
  };
  try {
    await withValidationRunner(
      async () => evidence(),
      async ({ environment, probeCalls }) => {
        const { store } = await import("../state/store.svelte");
        expect(store.serverReadiness.get("self")!.state).toBe("ready");
        environment.setVisibility("hidden");
        timers.advance(90000);
        environment.setVisibility("visible");
        timers.advance(0);
        await settleMicrotasks();
        expect(probeCalls()).toBe(2);
        expect(discoveries).toBe(1);
        expect(timers.size()).toBe(0);
        environment.setVisibility("hidden");
        timers.advance(CONNECTION_FRESH_MS + 1);
        generation = "gen-b";
        expect(probeCalls()).toBe(2);
        environment.setVisibility("visible");
        expect(store.serverReadiness.get("self")!.state).not.toBe("ready");
        for (let turn = 0; turn < 5; turn++) {
          timers.advance(0);
          await settleMicrotasks();
        }
        expect(discoveries).toBe(2);
        expect(probeCalls()).toBe(4);
        expect(store.serverReadiness.get("self")!.state).toBe("ready");
        expect(store.serverDiscoveries.get("self")!.generation).toBe("gen-b");
        expect(
          store.serverValidation.get("self")!.latency.path!.generation,
        ).toBe("gen-b");
        expect(timers.size()).toBe(0);
      },
      undefined,
      testServerCatalog,
      async () => {
        discoveries++;
        return evidence().discovery;
      },
    );
  } finally {
    timers.restore();
  }
});

test("a disposed validation cannot restore discovery or evidence", async () => {
  let release: ((info: PreparedPaths) => void) | undefined;
  let defer = false;
  await withValidationRunner(
    () =>
      defer
        ? new Promise((resolve) => {
            release = resolve;
          })
        : Promise.resolve(PROBE_EVIDENCE),
    async ({ engine }) => {
      const { store } = await import("../state/store.svelte");
      defer = true;
      const pending = engine.validateConnections(true);
      await yieldUntil(() => release !== undefined);
      engine.dispose();
      release!(PROBE_EVIDENCE);
      await expect(pending).rejects.toMatchObject({ name: "AbortError" });
      expect(store.connectionValidation).toEqual(emptyConnectionValidation());
      expect(store.transportDiscovery).toBeNull();
    },
  );
});

test("live configuration rejects invalid plans before changing draft or runner", async () => {
  await withValidationRunner(
    async () => PROBE_EVIDENCE,
    async ({ engine, runner }) => {
      const { store } = await import("../state/store.svelte");
      const previous = JSON.parse(JSON.stringify(store.config)) as RunnerConfig;
      let reconfigured = 0;
      runner.reconfigure = () => {
        reconfigured++;
      };
      engine.toggleRun();
      await yieldUntil(() => runner.starts === 1);
      expect(
        engine.configureRun({
          duration: { ...previous.duration, uploadMs: -1 },
        }),
      ).toBe(false);
      expect(
        engine.configureRun({
          stages: {
            latency: false,
            download: false,
            upload: false,
            bidirectional: false,
          },
        }),
      ).toBe(false);
      expect(store.config).toEqual(previous);
      expect(store.activeConfig).toEqual(previous);
      expect(reconfigured).toBe(0);
      const duration = {
        ...previous.duration,
        uploadMs: previous.duration.uploadMs + 1000,
      };
      expect(engine.configureRun({ duration })).toBe(true);
      expect(store.config.duration).toEqual(duration);
      expect(store.activeConfig?.duration).toEqual(duration);
      expect(reconfigured).toBe(1);
      store.config = previous;
    },
  );
});

test("superseded preparation never replaces newer evidence and disposes its provisional monitor", async () => {
  let release: ((paths: PreparedPaths) => void) | undefined;
  let deferred = false;
  await withValidationRunner(
    () =>
      deferred
        ? new Promise((resolve) => {
            release = resolve;
          })
        : Promise.resolve(testPreparedPaths()),
    async ({ engine, idleStops }) => {
      const { store } = await import("../state/store.svelte");
      deferred = true;
      const stale = engine.validateConnections(true);
      await yieldUntil(() => release !== undefined);
      deferred = false;
      await engine.validateConnections(true);
      const committed = store.connectionValidation.throughput.path;
      const stopped = idleStops();
      const outdated = testPreparedPaths();
      outdated.discovery.generation = "outdated";
      release!(outdated);
      await expect(stale).rejects.toMatchObject({ name: "AbortError" });
      await settleMicrotasks();
      expect(store.transportDiscovery?.generation).toBe("gen-a");
      expect(store.connectionValidation.throughput.path).toBe(committed);
      expect(idleStops()).toBe(stopped + 1);
    },
  );
});

test("idle latency is stopped before the measurement runner starts and resumes after abort", async () => {
  await withValidationRunner(
    async () => testPreparedPaths(),
    async ({ engine, runner, idleActive }) => {
      expect(idleActive()).toBe(true);
      const start = runner.start.bind(runner);
      runner.start = () => {
        expect(idleActive()).toBe(false);
        start();
      };
      engine.toggleRun();
      await yieldUntil(() => runner.starts === 1);
      expect(idleActive()).toBe(false);
      engine.toggleRun();
      await settleValidation();
      expect(idleActive()).toBe(true);
    },
  );
});

test("returning to start releases the run so late events cannot reach the fresh store", async () => {
  await withValidationRunner(
    async () => testPreparedPaths(),
    async ({ engine, runner }) => {
      const { store } = await import("../state/store.svelte");
      engine.toggleRun();
      await yieldUntil(() => runner.starts === 1);
      const late = runner.listener;
      engine.returnToStart();
      expect(store.phase).toBe("idle");
      late({
        type: "serverLatencySummary",
        serverId: "self",
        stage: "download",
        summary: null,
      });
      runner.listener({
        type: "stageSkipped",
        failure: {
          stage: "download",
          reason: "connection-lost",
          message: "late",
        },
      });
      expect(store.summariesByServer.size).toBe(0);
      expect(store.stageFailures).toEqual({});
    },
  );
});

test("canceling an approval ignores a grant returned by an already pending exchange", async () => {
  const origin = new URL("https://ui.example/");
  let response: ((value: Response) => void) | undefined;
  let signal: AbortSignal | undefined;
  let discoveries = 0;
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    location: origin,
    window: {
      location: origin,
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent() {},
    },
    document: {
      visibilityState: "visible",
      addEventListener() {},
      removeEventListener() {},
    },
    navigator: { onLine: true },
    fetch: (_input: RequestInfo | URL, init?: RequestInit) => {
      signal = init?.signal ?? undefined;
      // A response already in flight may finish after cancellation.
      return new Promise<Response>((resolve) => (response = resolve));
    },
  });
  const { createApplicationController } = await import("./engine.svelte");
  const { store } = await import("../state/store.svelte");
  const catalog = store.serverCatalog;
  const selection = store.selectedServers;
  store.reset();
  store.serverCatalog = {
    defaultSelection: ["peer"],
    servers: [{ id: "peer", name: "Private", url: "https://peer.example" }],
  };
  store.selectedServers = ["peer"];
  const engine = createApplicationController(store, {
    discover: async () => {
      discoveries++;
      return testServerDiscovery();
    },
  });
  try {
    const pending = engine.signInServer("peer");
    while (!response) await Bun.sleep(1);
    expect(store.serverApproval?.code).toMatch(/^[A-Z2-7]{8}$/);
    engine.cancelServerApproval();
    expect(signal?.aborted).toBe(true);
    response(Response.json({ token: "a".repeat(43), remainingMs: 60_000 }));
    await pending;
    expect(store.serverApproval).toBeNull();
    expect(discoveries).toBe(0);
    expect(store.isRunning).toBe(false);
  } finally {
    engine.dispose();
    store.serverCatalog = catalog;
    store.selectedServers = selection;
    store.reset();
    restore();
  }
});

test("pending approval excludes Start and catalogue replacement cancels its old-origin exchange", async () => {
  const origin = new URL("https://ui.example/");
  let response: ((value: Response) => void) | undefined;
  let signal: AbortSignal | undefined;
  let peerUrl = "https://peer.example";
  const discovered: { url: string; kind: string }[] = [];
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    location: origin,
    window: {
      location: origin,
      open: () => null,
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent() {},
    },
    document: {
      visibilityState: "visible",
      querySelector: () => null,
      addEventListener() {},
      removeEventListener() {},
    },
    navigator: { onLine: true },
    fetch: (input: RequestInfo | URL, init?: RequestInit) => {
      expect(String(input)).toBe("https://peer.example/auth/browser/token");
      signal = init?.signal ?? undefined;
      return new Promise<Response>((resolve) => (response = resolve));
    },
  });
  const { createApplicationController } = await import("./engine.svelte");
  const { store } = await import("../state/store.svelte");
  const catalog = store.serverCatalog;
  const selection = store.selectedServers;
  store.reset();
  const engine = createApplicationController(store, {
    loadCatalog: async () => ({
      defaultSelection: ["peer"],
      servers: [
        { id: "self", name: "Home", url: origin.origin },
        { id: "peer", name: "Private", url: peerUrl },
      ],
    }),
    discover: async (_signal, credentials) => {
      discovered.push({
        url: credentials!.server.url,
        kind: credentials!.kind,
      });
      throw new Error("Sign-in is required");
    },
  });
  try {
    await engine.boot();
    const pending = engine.signInServer("peer");
    while (!response) await Bun.sleep(1);
    engine.toggleRun();
    expect(store.preparationStatus).toBe("blocked");
    expect(store.startError).toContain("Finish signing in");
    expect(engine.hasPendingStart()).toBe(false);
    peerUrl = "https://replacement.example";
    await engine.retryCatalogue();
    expect(signal?.aborted).toBe(true);
    response(Response.json({ token: "a".repeat(43), remainingMs: 60_000 }));
    await pending;
    expect(store.serverApproval).toBeNull();
    await engine.validateConnections(true).catch(() => {});
    expect(discovered.at(-1)).toEqual({ url: peerUrl, kind: "public" });
    expect(store.preparing).toBe(false);
  } finally {
    engine.dispose();
    store.serverCatalog = catalog;
    store.selectedServers = selection;
    store.reset();
    restore();
  }
});
