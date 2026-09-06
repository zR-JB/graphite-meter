import "../state/runes.test";
import { stubGlobals } from "../test-helpers.test";
import { expect, test } from "bun:test";
import {
  TEST_BUILD_TOKENS,
  testPreparedPaths,
  testServerCatalog,
  testServerDiscovery,
} from "./test-helpers.test";
import type { NetworkRunner, RunnerEvent } from "./contract";

test("a canceled start cannot overwrite the newer run's session budget when authentication resolves late", async () => {
  const saved = new Map<string, PropertyDescriptor | undefined>();
  function stub(key: string, value: unknown) {
    saved.set(key, Object.getOwnPropertyDescriptor(globalThis, key));
    Object.defineProperty(globalThis, key, {
      value,
      configurable: true,
      writable: true,
    });
  }
  for (const [key, value] of Object.entries(TEST_BUILD_TOKENS))
    stub(key, value);
  stub("window", {
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() {},
  });
  stub("document", {
    visibilityState: "visible",
    cookie: "",
    querySelector: () => ({ getAttribute: () => "enabled" }),
    addEventListener() {},
    removeEventListener() {},
  });
  stub("navigator", { onLine: true });
  stub("location", new URL("https://meter.test/"));
  const authRequests: {
    signal: AbortSignal;
    finish: (remainingMs: number) => void;
  }[] = [];
  stub("fetch", (input: RequestInfo | URL, init?: RequestInit) => {
    expect(String(input)).toBe("/auth/session");
    return new Promise<Response>((resolve) =>
      authRequests.push({
        signal: init!.signal!,
        finish: (remainingMs) =>
          resolve(Response.json({ remainingMs, maximumLifetimeMs: 1_000_000 })),
      }),
    );
  });
  const { createApplicationController } = await import("./engine.svelte");
  const { store } = await import("../state/store.svelte");
  const previous = JSON.parse(JSON.stringify(store.config));
  store.config.stages = {
    latency: false,
    download: true,
    upload: false,
    bidirectional: false,
  };
  store.config.skipLoadedLatencyWhenStageOff = true;
  store.config.duration = {
    warmupMs: 0,
    latencyMs: 0,
    downloadMs: 1000,
    uploadMs: 0,
    bidirectionalMs: 0,
  };
  let listener: (event: RunnerEvent) => void = () => {};
  let starts = 0;
  const runner: NetworkRunner = {
    phase: "idle",
    start() {
      starts++;
      listener({
        type: "phase",
        transition: { from: "idle", to: "download", stage: "download", t: 0 },
      });
    },
    abort() {},
    dispose() {},
    reconfigure() {},
    on(next) {
      listener = next;
      return () => {};
    },
  };
  const engine = createApplicationController(store, {
    loadCatalog: testServerCatalog,
    discover: testServerDiscovery,
    createRunner: () => runner,
    prepare: async (config) => {
      const paths = testPreparedPaths({ latency: null });
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
            state: "stale",
            path: null,
          },
        },
      };
    },
  });
  async function settle() {
    for (let i = 0; i < 20; i++) await Promise.resolve();
  }
  try {
    await engine.boot();
    engine.toggleRun();
    expect(authRequests).toHaveLength(1);
    engine.cancelPendingStart();
    expect(authRequests[0].signal.aborted).toBe(true);
    engine.toggleRun();
    expect(authRequests).toHaveLength(2);
    authRequests[1].finish(100_000);
    await settle();
    expect(starts).toBe(1);
    authRequests[0].finish(900_000);
    await settle();
    expect(starts).toBe(1);
    expect(
      engine.configureRun({
        duration: { ...store.config.duration, downloadMs: 300_000 },
      }),
    ).toBe(false);
    expect(store.startError).toBe(
      "This change would extend the test beyond the current session.",
    );
    expect(store.activeConfig?.duration.downloadMs).toBe(1000);
  } finally {
    engine.dispose();
    store.config = previous;
    for (const [key, descriptor] of saved) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else Reflect.deleteProperty(globalThis, key);
    }
  }
});

test("cancel, deselection, and disposal close owned approval popups before delayed setup can return", async () => {
  const popups: {
    closed: boolean;
    opener: object | null;
    location: { replace: (url: string) => void };
    close: () => void;
  }[] = [];
  let navigations = 0;
  let requests = 0;
  const origin = new URL("https://ui.example/");
  const restore = stubGlobals({
    ...TEST_BUILD_TOKENS,
    location: origin,
    window: {
      location: origin,
      open() {
        const popup = {
          closed: false,
          opener: {},
          location: {
            replace() {
              navigations++;
            },
          },
          close() {
            this.closed = true;
          },
        };
        popups.push(popup);
        return popup;
      },
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
    localStorage: { setItem() {} },
    fetch: async () => {
      requests++;
      return Response.json({}, { status: 202 });
    },
  });
  const { createApplicationController } = await import("./engine.svelte");
  const { store } = await import("../state/store.svelte");
  const catalog = store.serverCatalog;
  const selection = store.selectedServers;
  const latency = store.latencySelection;
  store.reset();
  store.serverCatalog = {
    defaultSelection: ["self"],
    servers: [
      { id: "self", name: "Home", url: origin.origin },
      { id: "peer", name: "Private", url: "https://peer.example" },
    ],
  };
  store.selectedServers = ["self", "peer"];
  const engine = createApplicationController(store);
  try {
    const canceled = engine.signInServer("peer");
    engine.cancelServerApproval();
    await canceled;
    expect(popups[0].closed).toBe(true);
    expect(popups[0].opener).toBeNull();
    const deselected = engine.signInServer("peer");
    engine.applyServers(["self"]);
    await deselected;
    expect(popups[1].closed).toBe(true);
    expect(popups[1].opener).toBeNull();
    const disposed = engine.signInServer("peer");
    engine.dispose();
    await disposed;
    expect(popups[2].closed).toBe(true);
    expect(popups[2].opener).toBeNull();
    expect(store.serverApproval).toBeNull();
    expect(navigations).toBe(0);
    expect(requests).toBe(0);
  } finally {
    engine.dispose();
    store.serverCatalog = catalog;
    store.selectedServers = selection;
    store.latencySelection = latency;
    store.reset();
    restore();
  }
});
