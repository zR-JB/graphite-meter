// Contracts of the connection model and the application controller that owns it.
import "../state/runes.testutil";
import { afterAll, beforeAll, expect, spyOn, test } from "bun:test";
import type { FetchThroughputTarget, LatencyTarget } from "../api/endpoints";
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
  CONNECTION_FRESH_MS,
  emptyConnectionValidation,
  classifyTransportDiscovery,
  fetchViewOfOrigin,
  selectTarget,
  type ConnectionValidation,
  type ServerView,
} from "./paths";
import { presentConnections } from "../presentation/paths";
import type {
  ApplicationController,
  createApplicationController,
} from "./controller.svelte";
import type { ConnectionPreparation } from "./real/prepare";
import type { IdleEvent } from "./real/latencyChannel";
import type { ServerEntry } from "../servers/catalog";
import { ServerAuthenticationRequired } from "../servers/credentials";
import { DEFAULT_CONFIG } from "../state/defaults";
import { stubGlobals } from "../test-helpers.testutil";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
  testRunResult,
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
};
const latency: LatencyTarget = {
  id: "ws-http1-tls",
  origin: "https://meter.test:7247",
  transport: "websocket",
  protocol: "http1",
  tls: true,
};
function config(): RunnerConfig {
  return {
    ...structuredClone(DEFAULT_CONFIG),
    transferStreams: { mode: "auto", count: 6 },
  };
}
function makeDiscovery(): TransportDiscovery {
  return {
    ...classifyTransportDiscovery(
      [throughput],
      [latency],
      throughput.origin,
      true,
      "h2",
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
  const transfer = selectTarget(discovery, "throughput", "auto", true)!;
  const ping = selectTarget(discovery, "latency", "auto", true);
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
  changed.throughput[throughput.origin].targets[0].protocol = "http1";
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
  const servers = new Map([
    ["a", { discovery: paths.discovery, validation: a }],
    ["b", { discovery: paths.discovery, validation: b }],
  ]);
  const summary = (role: "throughput" | "latency", ids = ["a", "b"]) =>
    summarizeRoleValidation(config(), role, ids, servers);
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
  servers.set("b", {
    discovery: { ...paths.discovery, generation: "new" },
    validation: b,
  });
  expect(summary("latency")).toEqual({ state: "stale", verified: 1, total: 2 });
  expect(summary("latency", ["a", "missing"])).toEqual({
    state: "stale",
    verified: 1,
    total: 2,
  });
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
  const servers = new Map([
    ["self", { discovery: paths.discovery, validation }],
  ]);
  expect(summarizeRoleValidation(cfg, "throughput", ["self"], servers)).toEqual(
    { state: "failed", verified: 0, total: 1 },
  );
  expect(summarizeRoleValidation(cfg, "latency", ["self"], servers).state).toBe(
    "verified",
  );
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

/* ---------- Application controller ---------- */
type Dependencies = NonNullable<
  Parameters<typeof createApplicationController>[1]
>;
type Store = typeof import("../state/store.svelte").store;
interface Harness {
  controller: ApplicationController;
  store: Store;
  runner: TestRunner;
  idle: ReturnType<typeof idleMonitors>;
  setVisibility: (state: "hidden" | "visible") => void;
  view: (id?: string) => ServerView;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => (resolve = done));
  return { promise, resolve };
}
async function until(done: () => boolean, turns = 100): Promise<void> {
  for (let turn = 0; turn < turns && !done(); turn++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}
const settle = () => until(() => false, 10);
function evidence(generation = "gen-a"): PreparedPaths {
  const paths = testPreparedPaths();
  paths.discovery.generation = generation;
  paths.throughput.generation = generation;
  paths.latency!.generation = generation;
  return paths;
}
function preparation(
  config: RunnerConfig = DEFAULT_CONFIG,
  paths = evidence(),
): ConnectionPreparation {
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
  };
}
function idleMonitors() {
  let active = false;
  let stops = 0;
  let onEvent: (event: IdleEvent) => void = () => {};
  return {
    active: () => active,
    stops: () => stops,
    create: (): NonNullable<ConnectionPreparation["idle"]> => ({
      start() {
        active = true;
      },
      stop() {
        active = false;
        stops++;
      },
      get onEvent() {
        return onEvent;
      },
      set onEvent(value) {
        onEvent = value;
      },
    }),
  };
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
  details() {
    return testRunResult().multiServer;
  }
  on(listener: (event: RunnerEvent) => void) {
    this.listener = listener;
    return () => {
      this.listener = () => {};
    };
  }
}
function listeners() {
  const handlers = new Map<string, (event: Event) => void>();
  return {
    addEventListener(type: string, handler: (event: Event) => void) {
      handlers.set(type, handler);
    },
    removeEventListener(type: string) {
      handlers.delete(type);
    },
    dispatchEvent: () => true,
    emit: (type: string) => handlers.get(type)?.(new Event(type)),
  };
}
async function withController(
  options: Partial<Dependencies> & {
    origin?: string;
    servers?: (string | ServerEntry)[];
    selected?: string[];
    hidden?: boolean;
  },
  run: (harness: Harness) => Promise<void>,
): Promise<void> {
  const origin = new URL(options.origin ?? "http://meter.test/");
  const documentEvents = listeners();
  const document = {
    ...documentEvents,
    visibilityState: options.hidden ? "hidden" : "visible",
    querySelector: () => null,
  };
  const restore = stubGlobals({
    location: origin,
    window: { ...listeners(), location: origin, open: () => null },
    document,
    navigator: { onLine: true },
    fetch: () => Promise.reject(new Error("no network")),
  });
  const { createApplicationController } = await import("./controller.svelte");
  const { store } = await import("../state/store.svelte");
  store.restoreTestDisplayDefaults();
  const servers = (options.servers ?? ["self"]).map((server) =>
    typeof server === "string"
      ? { id: server, name: server, url: origin.origin }
      : server,
  );
  const idle = idleMonitors();
  const runner = new TestRunner();
  const controller = createApplicationController(store, {
    loadCatalog: async () => ({
      servers,
      defaultSelection: options.selected ?? [servers[0].id],
    }),
    discover: testServerDiscovery,
    prepare: async (config, _previous, roles) => ({
      ...preparation(config),
      idle: roles.includes("latency") ? idle.create() : undefined,
    }),
    createRunner: () => runner,
    ...options,
  });
  try {
    await controller.boot();
    await run({
      controller,
      store,
      runner,
      idle,
      setVisibility(state) {
        document.visibilityState = state;
        documentEvents.emit("visibilitychange");
      },
      view: (id = "self") => store.servers.get(id)!,
    });
  } finally {
    controller.dispose();
    restore();
  }
}
/** Completes a browser approval whose token exchange answers at once. */
async function approve(harness: Harness, id: string, remainingMs: number) {
  const restore = stubGlobals({
    fetch: async () => Response.json({ token: "a".repeat(43), remainingMs }),
  });
  try {
    await harness.controller.signInServer(id);
  } finally {
    restore();
  }
}
const remote = (id: string): ServerEntry => ({
  id,
  name: id,
  url: `https://${id}.example`,
});

test("a failed role preserves the independently verified role", async () => {
  await withController(
    {
      hidden: true,
      prepare: async (config, _previous, roles) => {
        if (roles.includes("latency")) throw new Error("offline");
        return preparation(config);
      },
    },
    async ({ controller, view }) => {
      await expect(controller.validateConnections()).rejects.toThrow();
      expect(view().validation.throughput.state).toBe("verified");
      expect(view().validation.latency.state).toBe("failed");
      expect(view().readiness).toBe("failed");
    },
  );
});

test("wrapped remote authentication retains actionable sign-in details", async () => {
  const { PreflightUnavailableError } = await import("./real/prepare");
  await withController(
    {
      hidden: true,
      servers: ["self", "peer"],
      selected: ["peer"],
      discover: async (_signal, context) => {
        throw new PreflightUnavailableError("preflight unavailable", {
          cause: new ServerAuthenticationRequired(context!.server),
        });
      },
    },
    async ({ controller, view }) => {
      await expect(controller.validateConnections()).rejects.toThrow(
        "Sign in to peer",
      );
      expect(view("peer")).toMatchObject({
        readiness: "sign-in",
        message: "Sign in to peer",
      });
    },
  );
});

test("equivalent selections keep their view; an expired reselection refreshes discovery and both roles", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  const checked: string[] = [];
  let discoveries = 0;
  try {
    await withController(
      {
        hidden: true,
        servers: ["self", "peer"],
        discover: async () => {
          discoveries++;
          return evidence().discovery;
        },
        prepare: async (config, _previous, roles) => {
          checked.push(...roles);
          return preparation(config);
        },
      },
      async ({ controller, setVisibility, view }) => {
        await controller.validateConnections();
        const ready = view();
        const { throughput, latency } = evidence();
        controller.selectConnection("throughput", throughput.target.origin);
        controller.selectConnection("latency", latency!.target.origin);
        await controller.validateConnections();
        expect(checked).toEqual(["throughput", "latency"]);
        expect(view().readiness).toBe("ready");
        expect(view().validation.throughput.path).toBe(
          ready.validation.throughput.path,
        );
        const equivalent = view();
        controller.selectConnection("latency", latency!.target.origin);
        expect(view()).toBe(equivalent);
        controller.applyServers(["peer"]);
        clock.mockReturnValue(1000 + CONNECTION_FRESH_MS + 1);
        controller.applyServers(["self"]);
        expect(view().readiness).toBe("unchecked");
        setVisibility("visible");
        await until(() => view().readiness === "ready");
        expect(discoveries).toBe(2);
        expect(checked).toEqual([
          "throughput",
          "latency",
          "throughput",
          "latency",
        ]);
      },
    );
  } finally {
    clock.mockRestore();
  }
});

test("a generation change cancels an in-flight role before accepting replacement evidence", async () => {
  const held = deferred<ConnectionPreparation>();
  let generation = "gen-a";
  let block = false;
  let oldSignal: AbortSignal | undefined;
  await withController(
    {
      hidden: true,
      discover: async () => evidence(generation).discovery,
      prepare: async (config, _previous, roles, signal) => {
        if (block && roles.includes("latency")) {
          oldSignal = signal;
          return held.promise;
        }
        return preparation(config, evidence(generation));
      },
    },
    async ({ controller, view }) => {
      await controller.validateConnections();
      block = true;
      const old = controller
        .validateConnections(true, "latency")
        .catch((error) => error);
      await settle();
      expect(oldSignal?.aborted).toBe(false);
      generation = "gen-b";
      block = false;
      await controller.validateConnections(true, "throughput");
      expect((await old).name).toBe("AbortError");
      expect(oldSignal?.aborted).toBe(true);
      expect(view().readiness).toBe("ready");
      held.resolve(preparation());
      await settle();
      expect(view().validation.latency.path!.generation).toBe("gen-b");
    },
  );
});

test("dropping latency cancels its probe, and re-enabling cannot adopt the late evidence", async () => {
  const held = deferred<ConnectionPreparation>();
  let latencyChecks = 0;
  let stopped = 0;
  await withController(
    {
      hidden: true,
      prepare: async (config, _previous, roles) => {
        if (roles.includes("latency") && ++latencyChecks === 1)
          return held.promise;
        return preparation(config);
      },
    },
    async ({ controller, store, view }) => {
      const stages = { ...store.config.stages };
      const old = controller.validateConnections().catch((error) => error);
      await settle();
      expect(view().validation.throughput.state).toBe("verified");
      controller.configureRun({ stages: { ...stages, latency: false } });
      expect((await old).name).toBe("AbortError");
      expect(view().readiness).toBe("ready");
      controller.configureRun({ stages });
      expect(view().readiness).not.toBe("ready");
      await controller.validateConnections();
      const current = view().validation.latency.path;
      held.resolve({
        ...preparation(),
        idle: {
          start() {},
          stop: () => void stopped++,
          onEvent() {},
        },
      });
      await settle();
      expect(latencyChecks).toBe(2);
      expect(stopped).toBe(1);
      expect(view().validation.latency.path).toBe(current);
      expect(view().readiness).toBe("ready");
    },
  );
});

test("an expired grant needs a new approval even when its paths are fresh", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  try {
    await withController(
      {
        hidden: true,
        origin: "https://ui.example",
        servers: [remote("home"), remote("peer")],
        selected: ["peer"],
      },
      async (harness) => {
        const { controller, view } = harness;
        await approve(harness, "peer", 1000);
        await controller.validateConnections();
        expect(view("peer").readiness).toBe("ready");
        controller.applyServers(["home"]);
        clock.mockReturnValue(2001);
        controller.applyServers(["peer"]);
        expect(view("peer")).toMatchObject({
          readiness: "sign-in",
          message: "Sign in to node-a",
        });
        await approve(harness, "peer", 60_000);
        await controller.validateConnections();
        expect(view("peer").readiness).toBe("ready");
      },
    );
  } finally {
    clock.mockRestore();
  }
});

test("slow background discovery leaves capacity for a newly selected server", async () => {
  const background = deferred<void>();
  const started: string[] = [];
  try {
    await withController(
      {
        servers: ["slow-a", "slow-b", "selected"],
        selected: ["slow-a"],
        hidden: true,
        discover: async (_signal, credentials) => {
          started.push(credentials!.server.id);
          if (credentials!.server.id !== "selected") await background.promise;
          return evidence().discovery;
        },
      },
      async ({ controller, setVisibility, view }) => {
        controller.applyServers(["selected"]);
        controller.loadServerMetadata();
        setVisibility("visible");
        await until(() => started.length > 1);
        expect(started).toEqual(["slow-a", "selected"]);
        await controller.validateConnections();
        expect(view("selected").readiness).toBe("ready");
        expect(view("slow-a").metadataChecking).toBe(true);
      },
    );
  } finally {
    background.resolve();
  }
});

test("switching servers carries transport preferences and clears the old server's paths", async () => {
  await withController(
    { servers: ["self", remote("peer")] },
    async ({ controller, store }) => {
      const { throughput, latency } = evidence();
      controller.selectConnection("throughput", throughput.target.id);
      controller.selectConnection("latency", latency!.target.id);
      expect(controller.applyServers(["peer"])).toBe(true);
      expect(store.config.transports).toEqual({
        throughputTarget: "protocol:http1",
        latencyTarget: "transport:websocket",
      });
      expect(store.transportDiscovery).toBeNull();
      expect(store.connectionValidation.throughput.path).toBeNull();
    },
  );
});

test("a second Start click cancels its checks without blocking a later check", async () => {
  const held = deferred<ConnectionPreparation>();
  const signals: AbortSignal[] = [];
  let block = true;
  await withController(
    {
      hidden: true,
      prepare: async (config, _previous, _roles, signal) => {
        signals.push(signal!);
        return block ? held.promise : preparation(config);
      },
    },
    async ({ controller, store, runner, view }) => {
      controller.toggleRun();
      expect(controller.hasPendingStart()).toBe(true);
      expect(store.preparing).toBe(true);
      await until(() => signals.length === 2);
      controller.toggleRun();
      expect(controller.hasPendingStart()).toBe(false);
      expect(store.preparationStatus).toBe("idle");
      expect(store.startError).toBe("");
      expect(signals.every((signal) => signal.aborted)).toBe(true);
      block = false;
      await controller.validateConnections();
      expect(view().readiness).toBe("ready");
      held.resolve(preparation());
      expect(runner.starts).toBe(0);
      expect(store.phase).toBe("idle");
    },
  );
});

test("a failed Start check stays idle instead of manufacturing a run error", async () => {
  await withController(
    {
      hidden: true,
      discover: async () => {
        throw new Error("offline");
      },
    },
    async ({ controller, store }) => {
      controller.toggleRun();
      await until(() => !store.preparing);
      expect(store.phase).toBe("idle");
      expect(store.startError).toBe("Connection check failed");
      expect(store.preparationStatus).toBe("failed");
    },
  );
});

test("superseding a Start's check cannot leave the application preparing", async () => {
  let calls = 0;
  const held = deferred<void>();
  await withController(
    {
      prepare: async (config) => {
        if (++calls === 3) await held.promise;
        return preparation(config);
      },
    },
    async ({ controller, store, runner }) => {
      const { throughput } = store.servers.get("self")!.validation;
      throughput.path!.verifiedAt = Date.now() - CONNECTION_FRESH_MS - 1;
      controller.toggleRun();
      await until(() => calls === 3);
      expect(store.preparing).toBe(true);
      await controller.validateConnections(true);
      held.resolve();
      await until(() => !controller.hasPendingStart());
      expect(store.preparationStatus).toBe("idle");
      expect(runner.starts).toBe(0);
      controller.toggleRun();
      await until(() => runner.starts > 0);
      expect(runner.starts).toBe(1);
    },
  );
});

test("visibility resume reuses fresh checks and refreshes expired discovery after a server restart", async () => {
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  let generation = "gen-a";
  let discoveries = 0;
  let probes = 0;
  try {
    await withController(
      {
        discover: async () => {
          discoveries++;
          return evidence(generation).discovery;
        },
        prepare: async (config) => {
          probes++;
          return preparation(config, evidence(generation));
        },
      },
      async ({ setVisibility, view }) => {
        expect(view().readiness).toBe("ready");
        setVisibility("hidden");
        clock.mockReturnValue(91_000);
        setVisibility("visible");
        await settle();
        expect([discoveries, probes]).toEqual([1, 2]);
        setVisibility("hidden");
        clock.mockReturnValue(92_000 + CONNECTION_FRESH_MS);
        generation = "gen-b";
        setVisibility("visible");
        expect(view().readiness).not.toBe("ready");
        await until(() => view().readiness === "ready");
        expect([discoveries, probes]).toEqual([2, 4]);
        expect(view().discovery!.generation).toBe("gen-b");
        expect(view().validation.latency.path!.generation).toBe("gen-b");
      },
    );
  } finally {
    clock.mockRestore();
  }
});

test("disposal discards a pending check and its evidence", async () => {
  const held = deferred<ConnectionPreparation>();
  let defer = false;
  await withController(
    {
      prepare: async (config) => (defer ? held.promise : preparation(config)),
    },
    async ({ controller, store }) => {
      defer = true;
      const pending = controller.validateConnections(true);
      await settle();
      controller.dispose();
      held.resolve(preparation());
      await expect(pending).rejects.toMatchObject({ name: "AbortError" });
      expect(store.servers.size).toBe(0);
      expect(store.connectionValidation).toEqual(emptyConnectionValidation());
      expect(store.transportDiscovery).toBeNull();
    },
  );
});

test("superseded preparation never replaces newer evidence and disposes its monitor", async () => {
  const held = deferred<ConnectionPreparation>();
  let defer = false;
  let stopped = 0;
  await withController(
    {
      prepare: async (config, _previous, roles) =>
        defer && roles.includes("latency") ? held.promise : preparation(config),
    },
    async ({ controller, store }) => {
      defer = true;
      const stale = controller.validateConnections(true);
      await settle();
      defer = false;
      await controller.validateConnections(true);
      const committed = store.connectionValidation.throughput.path;
      held.resolve({
        ...preparation(DEFAULT_CONFIG, evidence("outdated")),
        idle: { start() {}, stop: () => void stopped++, onEvent() {} },
      });
      await expect(stale).rejects.toMatchObject({ name: "AbortError" });
      await settle();
      expect(store.transportDiscovery?.generation).toBe("gen-a");
      expect(store.connectionValidation.throughput.path).toBe(committed);
      expect(stopped).toBe(1);
    },
  );
});

test("live configuration rejects invalid plans before changing draft or runner", async () => {
  await withController({}, async ({ controller, store, runner }) => {
    const previous: RunnerConfig = JSON.parse(JSON.stringify(store.config));
    let reconfigured = 0;
    runner.reconfigure = () => void reconfigured++;
    controller.toggleRun();
    await until(() => runner.starts === 1);
    const stages = { ...previous.stages };
    for (const stage of Object.keys(stages) as (keyof typeof stages)[])
      stages[stage] = false;
    const negative = { ...previous.duration, uploadMs: -1 };
    expect(controller.configureRun({ duration: negative })).toBe(false);
    expect(controller.configureRun({ stages })).toBe(false);
    expect(store.config).toEqual(previous);
    expect(store.activeConfig).toEqual(previous);
    expect(reconfigured).toBe(0);
    const duration = { ...previous.duration, uploadMs: 11_000 };
    expect(controller.configureRun({ duration })).toBe(true);
    expect(store.activeConfig?.duration).toEqual(duration);
    expect(reconfigured).toBe(1);
  });
});

test("idle latency stops before the run starts and resumes after abort", async () => {
  await withController({}, async ({ controller, runner, idle }) => {
    expect(idle.active()).toBe(true);
    const start = runner.start.bind(runner);
    runner.start = () => {
      expect(idle.active()).toBe(false);
      start();
    };
    controller.toggleRun();
    await until(() => runner.starts === 1);
    expect(idle.active()).toBe(false);
    controller.toggleRun();
    expect(idle.active()).toBe(true);
  });
});

test("returning to start releases the run so late events cannot reach the fresh store", async () => {
  await withController({}, async ({ controller, store, runner }) => {
    controller.toggleRun();
    await until(() => runner.starts === 1);
    const late = runner.listener;
    controller.returnToStart();
    expect(store.phase).toBe("idle");
    late({
      type: "serverFailure",
      failure: {
        serverId: "self",
        stage: "download",
        atMs: 0,
        scope: "throughput",
        reason: "connection-lost",
        message: "",
      },
      participants: [],
    });
    expect(store.stageFailures).toEqual({});
  });
});

test("an approval in flight blocks Start; cancellation or a new catalogue ignores its grant", async () => {
  let peerUrl = "https://peer.example";
  const discovered: string[] = [];
  const exchanges: { url: string; signal: AbortSignal }[] = [];
  const responses: ((value: Response) => void)[] = [];
  const grant = () =>
    responses.shift()!(
      Response.json({ token: "a".repeat(43), remainingMs: 60_000 }),
    );
  await withController(
    {
      hidden: true,
      origin: "https://ui.example",
      loadCatalog: async () => ({
        defaultSelection: ["peer"],
        servers: [{ id: "peer", name: "Private", url: peerUrl }],
      }),
      discover: async (_signal, credentials) => {
        discovered.push(`${credentials!.kind} ${credentials!.server.url}`);
        throw new Error("Sign-in is required");
      },
    },
    async ({ controller, store }) => {
      const restore = stubGlobals({
        fetch: (input: RequestInfo | URL, init?: RequestInit) => {
          exchanges.push({ url: String(input), signal: init!.signal! });
          return new Promise<Response>((resolve) => responses.push(resolve));
        },
      });
      try {
        let pending = controller.signInServer("peer");
        await until(() => exchanges.length === 1);
        expect(store.serverApproval?.code).toMatch(/^[A-Z2-7]{8}$/);
        controller.toggleRun();
        expect(store.preparationStatus).toBe("blocked");
        expect(store.startError).toContain("Finish signing in");
        expect(controller.hasPendingStart()).toBe(false);
        controller.cancelServerApproval();
        expect(exchanges[0].signal.aborted).toBe(true);
        grant();
        await pending;
        expect(store.serverApproval).toBeNull();
        expect(discovered).toEqual([]);
        pending = controller.signInServer("peer");
        await until(() => exchanges.length === 2);
        peerUrl = "https://replacement.example";
        await controller.retryCatalogue();
        expect(exchanges[1]).toMatchObject({
          url: "https://peer.example/auth/browser/token",
          signal: { aborted: true },
        });
        grant();
        await pending;
        expect(store.serverApproval).toBeNull();
        expect(discovered).toEqual(["public https://replacement.example"]);
      } finally {
        restore();
      }
    },
  );
});
