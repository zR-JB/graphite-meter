// Application controller contracts: selection, approval, run start/stop and the store it writes.
import "../state/runes.testutil";
import { afterAll, beforeAll, expect, spyOn, test } from "bun:test";
import type { RunnerConfig, RunnerEvent } from "./contract";
import { CONNECTION_FRESH_MS, type ServerView } from "./paths";
import { buildSegments } from "./schedule";
import type {
  ApplicationController,
  createApplicationController,
  Runner,
} from "./controller.svelte";
import type { ConnectionPreparation } from "./real/prepare";
import type { ServerEntry } from "../servers/catalog";
import { stubGlobals } from "../test-helpers.testutil";
import {
  deferred,
  settle,
  TEST_BUILD_TOKENS,
  testEvidence as evidence,
  testPreparation as preparation,
  testRunResult,
  testServerDiscovery,
  until,
} from "./test-helpers.testutil";

// Store and runner modules read build tokens when they first load.
let restoreBuild: () => void;
beforeAll(() => (restoreBuild = stubGlobals(TEST_BUILD_TOKENS)));
afterAll(() => restoreBuild());

type Dependencies = NonNullable<
  Parameters<typeof createApplicationController>[1]
>;
type Store = typeof import("../state/store.svelte").store;
interface Harness {
  controller: ApplicationController;
  store: Store;
  runner: TestRunner;
  idle: () => boolean;
  setVisibility: (state: "hidden" | "visible") => void;
  view: (id?: string) => ServerView;
}

class TestRunner implements Runner {
  listener: (event: RunnerEvent) => void = () => {};
  starts = 0;
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
  let idle = false;
  const runner = new TestRunner();
  const controller = createApplicationController(store, {
    loadCatalog: async () => ({
      servers,
      defaultSelection: options.selected ?? [servers[0].id],
    }),
    discover: testServerDiscovery,
    prepare: async (config, _previous, roles) => ({
      ...preparation(config),
      idle: roles.includes("latency")
        ? {
            start: () => void (idle = true),
            stop: () => void (idle = false),
            onEvent() {},
          }
        : undefined,
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
      idle: () => idle,
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

test("a pending start ends on a second click or a draft change without blocking later checks", async () => {
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

test("a cancelled start keeps the previous result on screen", async () => {
  let hold: Promise<void> | undefined;
  await withController(
    {
      discover: async () => {
        await hold;
        return evidence().discovery;
      },
    },
    async ({ controller, store, runner }) => {
      controller.toggleRun();
      await until(() => runner.starts === 1);
      runner.listener({ type: "complete", result: testRunResult() });
      const previous = store.result;
      expect(previous).not.toBeNull();
      const gate = deferred<void>();
      hold = gate.promise;
      controller.toggleRun();
      await until(() => store.preparationStatus === "checking");
      controller.toggleRun();
      gate.resolve();
      await settle();
      expect(store.result).toBe(previous);
      expect(runner.starts).toBe(1);
    },
  );
});

test("a stream plan that cannot fit blocks Start before the click", async () => {
  await withController(
    { servers: ["self", "peer"], selected: ["self", "peer"] },
    async ({ controller, store, runner }) => {
      expect(store.startBlocker).toBe("");
      controller.configureRun({
        transferStreams: { mode: "forced", count: 12 },
      });
      expect(store.startBlocker).toContain("Forced streams");
      expect(store.preparation.status).toBe("blocked");
      controller.toggleRun();
      await settle();
      expect(runner.starts).toBe(0);
      expect(store.startError).toContain("Forced streams");
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
    expect(controller.configureRun({ duration: negative })).toBe(false);
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
  const clock = spyOn(Date, "now").mockReturnValue(1000);
  let discoveries = 0;
  try {
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
        clock.mockReturnValue(2000 + CONNECTION_FRESH_MS);
        setVisibility("visible");
        expect(view().readiness).not.toBe("verified");
        await until(() => view().readiness === "verified");
        expect(discoveries).toBe(2);
      },
    );
  } finally {
    clock.mockRestore();
  }
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
        await controller.retry();
        expect(view("peer").readiness).toBe("verified");
        controller.applyServers(["home"]);
        clock.mockReturnValue(2001);
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
  } finally {
    clock.mockRestore();
  }
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
