// Application controller contracts: selection, approval, run start/stop and the store it writes.
import "../state/runes.testutil";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  expect,
  jest,
  test,
} from "bun:test";
import type { FailureReason, RunnerConfig, RunnerEvent } from "./contract";
import { AUTHENTICATION_REQUIRED_EVENT } from "../auth";
import { CONNECTION_FRESH_MS, type ServerView } from "./paths";
import { buildSegments } from "./schedule";
import type {
  ApplicationController,
  createApplicationController,
  Runner,
} from "./controller.svelte";
import type { ConnectionPreparation } from "./real/prepare";
import type { ServerEntry } from "../servers/catalog";
import { settle, stubGlobals, until } from "../test-helpers.testutil";
import {
  NOT_RUN,
  TEST_BUILD_TOKENS,
  testEvidence as evidence,
  testPreparation as preparation,
  testRunResult,
  testServerDiscovery,
} from "./test-helpers.testutil";

// Store and runner modules read build tokens when they first load.
let restoreBuild: () => void;
beforeAll(() => (restoreBuild = stubGlobals(TEST_BUILD_TOKENS)));
afterAll(() => restoreBuild());
beforeEach(() => jest.useFakeTimers());
afterEach(() => jest.useRealTimers());

type Dependencies = NonNullable<
  Parameters<typeof createApplicationController>[1]
>;
type Store = typeof import("../state/store.svelte").store;
interface Harness {
  controller: ApplicationController;
  store: Store;
  runner: TestRunner;
  idle: () => boolean;
  /** The servers probed between runs. */
  probed: () => string[];
  setVisibility: (state: "hidden" | "visible") => void;
  view: (id?: string) => ServerView;
  emit: (type: string) => void;
  navigated: string[];
}

class TestRunner implements Runner {
  listener: (event: RunnerEvent) => void = () => {};
  starts = 0;
  /** The server whose latency leads the run. */
  focus = "";
  start() {
    this.starts++;
    this.listener({
      type: "phase",
      transition: { to: "download", stage: "download", t: 0 },
    });
  }
  abort() {
    this.listener({
      type: "phase",
      transition: { to: "aborted", stage: null, t: 0 },
    });
  }
  ended: string[] = [];
  end(reason: FailureReason) {
    this.ended.push(reason);
    const failure = {
      serverId: "self",
      stage: "download" as const,
      atMs: 0,
      scope: "throughput" as const,
      reason,
      message: "",
    };
    const result = testRunResult({
      outcome: "incomplete",
      stages: { ...NOT_RUN, download: "failed" },
    });
    result.multiServer.failures = [failure];
    this.listener({ type: "complete", result });
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
  const navigated: string[] = [];
  const origin = Object.assign(
    new URL(options.origin ?? "http://meter.test/"),
    {
      replace: (url: string) => navigated.push(url),
    },
  );
  const documentEvents = listeners();
  const windowEvents = listeners();
  const document = {
    ...documentEvents,
    visibilityState: options.hidden ? "hidden" : "visible",
    querySelector: () => null,
  };
  const restore = stubGlobals({
    location: origin,
    window: { ...windowEvents, location: origin, open: () => null },
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
  const idle = new Set<string>();
  const runner = new TestRunner();
  const controller = createApplicationController(store, {
    loadCatalog: async () => ({
      servers,
      defaultSelection: options.selected ?? [servers[0].id],
    }),
    discover: testServerDiscovery,
    prepare: async (config, _previous, roles, _signal, { server }) => ({
      ...preparation(config),
      idle: roles.includes("latency")
        ? {
            start: () => void idle.add(server.id),
            stop: () => void idle.delete(server.id),
            onEvent() {},
          }
        : undefined,
    }),
    createRunner: (_prepared, focus) => {
      runner.focus = focus;
      return runner;
    },
    ...options,
  });
  try {
    await controller.boot();
    await run({
      controller,
      store,
      runner,
      idle: () => idle.size > 0,
      probed: () => [...idle],
      setVisibility(state) {
        document.visibilityState = state;
        documentEvents.emit("visibilitychange");
      },
      view: (id = "self") => store.servers.get(id)!,
      emit: windowEvents.emit,
      navigated,
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

test("a pending start ends on a second click or a draft change without blocking later checks", async () => {
  const held = Promise.withResolvers<ConnectionPreparation>();
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
      for (const [round, cancel] of [
        () => controller.toggleRun(),
        () => controller.selectConnection("latency", "transport:websocket"),
      ].entries()) {
        controller.toggleRun();
        expect(store.preparing).toBe(true);
        await until(() => signals.length === 2 * (round + 1));
        cancel();
        expect(controller.hasPendingStart()).toBe(false);
        expect(store.preparationStatus).toBe("idle");
        expect(store.startError).toBe("");
        expect(signals.every((signal) => signal.aborted)).toBe(true);
        await settle();
      }
      block = false;
      await controller.retry();
      expect(view().readiness).toBe("verified");
      held.resolve(preparation());
      expect(runner.starts).toBe(0);
      controller.toggleRun();
      await until(() => runner.starts > 0);
      expect(runner.starts).toBe(1);
    },
  );
});

test("a failed Start stays idle and lapses once its selection verifies", async () => {
  let offline = true;
  await withController(
    {
      hidden: true,
      discover: async () => {
        if (offline) throw new Error("offline");
        return testServerDiscovery();
      },
    },
    async ({ controller, store, setVisibility }) => {
      controller.toggleRun();
      await until(() => !store.preparing);
      expect(store.phase).toBe("idle");
      expect(store.startError).toBe("Connection check failed");
      expect(store.preparation.status).toBe("failed");
      offline = false;
      setVisibility("visible");
      await until(() => store.selectionValidation === "verified");
      expect(store.preparation.status).toBe("idle");
      expect(store.startError).toBe("");
    },
  );
});

test("the first selected server leads latency, not the fastest; one failing its start check is left out", async () => {
  let dead = false;
  const runner = new TestRunner();
  const started: Parameters<NonNullable<Dependencies["createRunner"]>>[] = [];
  await withController(
    {
      servers: ["self", remote("a"), remote("b")],
      selected: ["a", "b"],
      discover: async (_signal, credentials) => {
        if (dead && credentials.server.id === "a") throw new Error("offline");
        return testServerDiscovery();
      },
      prepare: async (config, _previous, _roles, _signal, credentials) => {
        const paths = evidence();
        paths.latency!.rttMs = credentials.server.id === "b" ? 1 : 40;
        return preparation(config, paths);
      },
      createRunner: (...args) => (started.push(args), runner),
    },
    async ({ controller, store }) => {
      controller.configureRun({
        stages: { ...store.config.stages, download: false, upload: false },
      });
      await until(() => store.selectionValidation === "verified");
      controller.toggleRun();
      await until(() => runner.starts === 1);
      expect([started[0][1], store.latencyFocus]).toEqual(["a", "a"]);
      controller.returnToStart();
      dead = true;
      controller.toggleRun();
      await until(() => runner.starts === 2);
      const [servers, focus, dropped] = started[1];
      expect(servers.map(({ server }) => server.id)).toEqual(["b"]);
      expect([focus, store.latencyFocus]).toEqual(["b", "b"]);
      expect(dropped).toMatchObject([
        { server: { id: "a" }, reason: "preparation-failed" },
      ]);
    },
  );
});

test("signing out mid-run saves the run before leaving for sign-in", async () => {
  await withController({}, async ({ controller, store, runner, ...page }) => {
    store.resultHistoryPreference = "enabled";
    controller.toggleRun();
    await until(() => runner.starts === 1);
    page.emit(AUTHENTICATION_REQUIRED_EVENT);
    page.emit(AUTHENTICATION_REQUIRED_EVENT);
    expect(runner.ended).toEqual(["sign-in-required"]);
    expect(store.historyCandidate?.result.outcome).toBe("incomplete");
    await settle();
    expect(page.navigated).toEqual([]);
    store.historyCandidate = null;
    await until(() => page.navigated.length > 0);
    expect(page.navigated).toEqual(["/login?reason=expired"]);
  });
});

test("a deliberate sign-out keeps its own landing, not the expired one", async () => {
  await withController({}, async ({ controller, ...page }) => {
    controller.signOut();
    page.emit(AUTHENTICATION_REQUIRED_EVENT);
    await settle();
    expect(page.navigated).toEqual([]);
  });
});

test("idle latency stops before the run starts and resumes after abort", async () => {
  await withController({}, async ({ controller, runner, idle }) => {
    expect(idle()).toBe(true);
    const start = runner.start.bind(runner);
    runner.start = () => {
      expect(idle()).toBe(false);
      start();
    };
    controller.toggleRun();
    await until(() => runner.starts === 1);
    expect(idle()).toBe(false);
    controller.toggleRun();
    expect(idle()).toBe(true);
  });
});

test("the shown latency server is the one probed between runs and leads the run", async () => {
  await withController(
    { servers: ["self", "peer"], selected: ["self", "peer"] },
    async ({ controller, store, runner, probed }) => {
      await until(() => store.selectionValidation === "verified");
      expect(probed()).toEqual(["self"]);
      controller.showLatency("peer");
      await until(() => probed().join() === "peer");
      expect(store.latencyFocus).toBe("peer");
      // The run's own details keep the chosen server's latency in view.
      controller.showServer("");
      expect(store.latencyFocus).toBe("peer");
      // Two servers on one origin share its HTTP/1.1 connections, too few for both transfers; latency alone runs.
      controller.toggleStage("download");
      controller.toggleStage("upload");
      await until(() => store.selectionValidation === "verified");
      controller.toggleRun();
      await until(() => runner.starts === 1);
      expect([runner.focus, store.latencyFocus]).toEqual(["peer", "peer"]);
    },
  );
});

test("without idle latency the connection settles from verified paths", async () => {
  await withController(
    { servers: ["self", "peer"], selected: ["self", "peer"] },
    async ({ controller, store, idle }) => {
      await until(() => store.selectionValidation === "verified");
      expect([idle(), store.effectiveConnectivity]).toEqual([
        true,
        "connected",
      ]);
      // A remote server first in the selection is probed between runs too, as its latency is the one shown.
      controller.applyServers(["peer"]);
      await until(() => store.selectionValidation === "verified");
      expect([idle(), store.effectiveConnectivity]).toEqual([
        true,
        "connected",
      ]);
      controller.applyServers(["self", "peer"]);
      controller.toggleStage("latency");
      await until(() => store.selectionValidation === "verified");
      expect([idle(), store.effectiveConnectivity]).toEqual([
        false,
        "connected",
      ]);
    },
  );
});

test("returning to start releases the run so late events cannot reach the fresh store", async () => {
  await withController({}, async ({ controller, store, runner }) => {
    controller.toggleRun();
    await until(() => runner.starts === 1);
    const late = runner.listener;
    controller.returnToStart();
    expect(store.phase).toBe("idle");
    late({
      type: "serverDetails",
      details: {
        selection: [],
        participants: [],
        latencyFocus: "self",
        intervals: [],
        omittedIntervals: 0,
        failures: [],
        servers: [],
      },
    });
    expect(store.serverDetails).toBeNull();
  });
});

test("a run keeps its RTT-adapted plan; live settings reject invalid plans", async () => {
  const slow = evidence();
  slow.latency!.rttMs = 300;
  const prepare: Dependencies["prepare"] = async (config) =>
    preparation(config, slow);
  await withController({ prepare }, async ({ controller, store, runner }) => {
    const previous: RunnerConfig = JSON.parse(JSON.stringify(store.config));
    const warmupMs = 3_000;
    const plan = { ...previous, duration: { ...previous.duration, warmupMs } };
    let reconfigured = 0;
    runner.reconfigure = () => void reconfigured++;
    controller.toggleRun();
    await until(() => runner.starts === 1);
    expect(store.totalEtaMs).toBe(buildSegments(plan).totalMs);
    const stages = { ...previous.stages };
    for (const stage of Object.keys(stages) as (keyof typeof stages)[])
      stages[stage] = false;
    const negative = { ...previous.duration, uploadMs: -1 };
    const skipped = { ...previous.duration, latencyMs: 0 };
    expect(controller.configureRun({ duration: negative })).toBe(false);
    expect(controller.configureRun({ duration: skipped })).toBe(false);
    expect(controller.configureRun({ stages })).toBe(false);
    expect(store.config).toEqual(previous);
    expect(store.run?.config).toEqual(plan);
    expect(reconfigured).toBe(0);
    const duration = { ...previous.duration, uploadMs: 11_000 };
    expect(controller.configureRun({ duration })).toBe(true);
    expect(store.run?.config.duration).toEqual({ ...duration, warmupMs });
    expect(reconfigured).toBe(1);
  });
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

test("hidden pages defer checks; returning refreshes discovery that expired meanwhile", async () => {
  jest.setSystemTime(1000);
  let discoveries = 0;
  await withController(
    {
      hidden: true,
      discover: async () => (discoveries++, evidence().discovery),
    },
    async ({ setVisibility, view }) => {
      expect(view().readiness).toBe("unchecked");
      setVisibility("visible");
      await until(() => view().readiness === "verified");
      setVisibility("hidden");
      jest.setSystemTime(2000 + CONNECTION_FRESH_MS);
      setVisibility("visible");
      expect(view().readiness).not.toBe("verified");
      await until(() => view().readiness === "verified");
      expect(discoveries).toBe(2);
    },
  );
});

test("an expired grant needs a new approval even when its paths are fresh", async () => {
  jest.setSystemTime(1000);
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
      await controller.retry();
      expect(view("peer").readiness).toBe("verified");
      controller.applyServers(["home"]);
      jest.setSystemTime(2001);
      controller.applyServers(["peer"]);
      expect(view("peer")).toMatchObject({
        readiness: "sign-in",
        message: "Sign in to node-a",
      });
      await approve(harness, "peer", 60_000);
      await controller.retry();
      expect(view("peer").readiness).toBe("verified");
    },
  );
});

test("an approval in flight blocks Start; cancellation or a new catalog ignores its grant", async () => {
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
        await controller.retryCatalog();
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
