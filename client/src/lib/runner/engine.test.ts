// engine.svelte.ts is a rune module: bun runs TypeScript but not the Svelte
// compiler, so the module is compiled on load here. It is imported with `window`
// absent so neither its own nor store.svelte.ts's top-level `$effect.root` block
// runs; bootRunner() needs no reactive root, only the DOM seams it touches.
import { test, expect } from "bun:test";
import { plugin, Transpiler } from "bun";
import { compileModule } from "svelte/compiler";
import { RunnerCore } from "./core";
import type { InfraInfo } from "./contract";
import {
  PreflightUnavailableError,
  TransportUnavailableError,
} from "./real/transportError";
import { CONNECTION_FRESH_MS } from "./connectionModel";

plugin({
  name: "svelte-runes",
  setup(build) {
    build.onLoad({ filter: /\.svelte\.ts$/ }, async (args) => {
      const source = await Bun.file(args.path).text();
      const module = new Transpiler({ loader: "ts" }).transformSync(source);
      return {
        contents: compileModule(module, {
          generate: "client",
          filename: args.path,
        }).js.code,
        loader: "js",
      };
    });
  },
});

const BUILD_TOKENS = {
  __GM_DEFAULT_ENGINE__: "real",
  __GM_ALLOW_DUMMY__: false,
  __GM_DEV_TOOLS__: false,
  __GM_BUILD_PROFILE__: "test",
  __GM_RELEASE_VERSION__: null,
  __GM_SOURCE_REVISION__: "test-revision",
  __GM_BUILD_IDENTITY__: "test test-revision",
  __GM_CLIENT_VERSION__: "0.0.0-test",
};

/** Install a global, returning the callback that puts the previous one back. */
function stubGlobal(key: string, value: unknown): () => void {
  const previous = Object.getOwnPropertyDescriptor(globalThis, key);
  Object.defineProperty(globalThis, key, {
    value,
    configurable: true,
    writable: true,
  });
  return () => {
    if (previous) Object.defineProperty(globalThis, key, previous);
    else Reflect.deleteProperty(globalThis, key);
  };
}

/** The DOM surface bootRunner touches, plus the visibility it reports. */
function stubBootEnvironment(visibility: "hidden" | "visible"): () => void {
  const restores = [
    stubGlobal("window", {
      addEventListener() {},
      removeEventListener() {},
    }),
    stubGlobal("document", {
      visibilityState: visibility,
      addEventListener() {},
      removeEventListener() {},
    }),
    // The boot probe is not under test; a refused preflight ends it at once.
    stubGlobal("fetch", () => Promise.reject(new Error("no network"))),
  ];
  return () => {
    for (const restore of restores.reverse()) restore();
  };
}

function stubEventBootEnvironment(
  visibility: "hidden" | "visible",
  online: boolean,
) {
  const windowListeners = new Map<string, () => void>();
  const documentListeners = new Map<string, () => void>();
  const documentState = {
    visibilityState: visibility,
    addEventListener(type: string, listener: () => void) {
      documentListeners.set(type, listener);
    },
    removeEventListener(type: string) {
      documentListeners.delete(type);
    },
  };
  const windowValue = {
    addEventListener(type: string, listener: () => void) {
      windowListeners.set(type, listener);
    },
    removeEventListener(type: string) {
      windowListeners.delete(type);
    },
  };
  const restores = [
    stubGlobal("window", windowValue),
    stubGlobal("document", documentState),
    stubGlobal("navigator", { onLine: online }),
  ];
  return {
    emit(type: string) {
      windowListeners.get(type)?.();
    },
    setVisibility(next: "hidden" | "visible") {
      documentState.visibilityState = next;
      documentListeners.get("visibilitychange")?.();
    },
    restore() {
      for (const restore of restores.reverse()) restore();
    },
  };
}

async function settleValidation(): Promise<void> {
  for (let turn = 0; turn < 10; turn++)
    await new Promise((resolve) => setTimeout(resolve, 0));
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

const PROBE_EVIDENCE: InfraInfo = {
  clientIp: "203.0.113.7",
  clientIpVersion: 4,
  clientIpSource: "socket",
  server: { name: "node-a", location: "Somewhere" },
  preTestPingMs: 12,
  engineVersion: "1.2.3",
  discoveryGeneration: "gen-a",
  protocolNegotiated: "h2",
  serverLoad: { active: 3, max: 4 },
};

// `store.infra` is the last probe's evidence: server identity, occupancy, and
// the pre-test ping. Four surfaces read it ungated by the connection
// presentation — the drawer's Server node and Location rows (via the
// `transportDiscovery?.server ?? infra?.server` fallback), its Server load row,
// the gauge's latency scale floor, and `store.liveRtt` — so evidence that
// outlives its session is rendered as if current.
//
// It is server-scoped, not run-scoped. `toggleRun()` calls `store.reset()` at the
// start of every run and re-probes only when the prepared probe went stale, so
// clearing it per run would blank those rows between runs against the same
// server. Teardown is where the server binding itself ends, which is why
// `transportDiscovery` is cleared there and not in `reset()`; the probe
// evidence is its peer and belongs beside it.
test("teardown clears the probe evidence; a run reset keeps it", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  // Imported with `window` absent, so neither top-level `$effect.root` block runs.
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  try {
    const { bootRunner, teardownRunner } = await import("./engine.svelte");
    const { store } = await import("../state/store.svelte");

    const restore = stubBootEnvironment("visible");
    await bootRunner();
    store.ingest({ type: "infra", info: PROBE_EVIDENCE });

    // A run starts by resetting the store; the endpoint drawer must survive it.
    store.reset();
    expect(store.infra).toEqual(PROBE_EVIDENCE);

    teardownRunner();
    restore();
    expect(store.infra).toBeNull();
  } finally {
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("store reset clears a transient start error", async () => {
  const { store } = await import("../state/store.svelte");
  store.startError = "This test would outlast the session.";
  store.preparation = {
    status: "checking",
    throughput: "checking",
    latency: "checking",
  };
  store.reset();
  expect(store.startError).toBe("");
  expect(store.preparation.status).toBe("idle");
});

test("an explicit second start click cancels a pending preflight", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  try {
    const { bootRunner, hasPendingStart, teardownRunner, toggleRun } =
      await import("./engine.svelte");
    const { store } = await import("../state/store.svelte");
    const restoreEnvironment = stubBootEnvironment("visible");
    await bootRunner();
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
    for (let turn = 0; turn < 10 && !pendingSignal; turn++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    toggleRun();
    expect(hasPendingStart()).toBe(false);
    expect(store.preparing).toBe(false);
    expect(pendingSignal?.aborted).toBe(true);
    expect(store.phase).toBe("idle");
    expect(store.startError).toBe("");
    expect(store.preparation.status).toBe("idle");
    restorePendingFetch();
    teardownRunner();
    restoreEnvironment();
  } finally {
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("a preflight failure stays idle instead of manufacturing a run error", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  try {
    const { bootRunner, teardownRunner, toggleRun } =
      await import("./engine.svelte");
    const { store } = await import("../state/store.svelte");
    const restoreEnvironment = stubBootEnvironment("visible");
    await bootRunner();
    const restoreFetch = stubGlobal("fetch", () =>
      Promise.reject(new Error("offline")),
    );
    store.reset();
    toggleRun();
    for (let turn = 0; turn < 10 && store.preparing; turn++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    expect(store.phase).toBe("idle");
    expect(store.startError).toBe("Connection check failed");
    expect(store.preparation.status).toBe("failed");
    restoreFetch();
    teardownRunner();
    restoreEnvironment();
  } finally {
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("connection failures use safe presentation copy", async () => {
  const { connectionFailureMessage } = await import("./engine.svelte");

  expect(
    connectionFailureMessage(
      new PreflightUnavailableError("preflight unavailable", {
        cause: { name: "TypeError", message: "Failed to fetch" },
      }),
    ),
  ).toBe("Server could not be reached");
  expect(
    connectionFailureMessage(
      new PreflightUnavailableError("preflight unavailable", {
        cause: { name: "NetworkError", message: "Network request failed" },
      }),
    ),
  ).toBe("Server could not be reached");
  expect(
    connectionFailureMessage(
      new PreflightUnavailableError("preflight unavailable", {
        cause: { name: "TypeError", message: "Unexpected programmer error" },
      }),
    ),
  ).toBe("Connection check failed");
  expect(
    connectionFailureMessage(
      new PreflightUnavailableError("preflight unavailable", {
        cause: new Error("preflight returned HTTP 503"),
      }),
    ),
  ).toBe("Connection check failed");
  expect(
    connectionFailureMessage(
      new TransportUnavailableError("throughput probe request failed", {
        cause: new TypeError("Failed to fetch"),
        role: "throughput",
      }),
    ),
  ).toBe("Connection check failed");
});

test("preparation names a disabled throughput path for latency-only runs", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  try {
    const { bootRunner, cancelPendingStart, teardownRunner, toggleRun } =
      await import("./engine.svelte");
    const { store } = await import("../state/store.svelte");
    const restoreEnvironment = stubBootEnvironment("visible");
    await bootRunner();
    const restoreFetch = stubGlobal(
      "fetch",
      (_input: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>(() => {
          void init;
        }),
    );
    const previousConfig = JSON.parse(JSON.stringify(store.config));
    store.config.stages = {
      latency: true,
      download: false,
      upload: false,
      bidirectional: false,
    };
    store.config.skipLoadedLatencyWhenStageOff = true;
    store.reset();
    toggleRun();
    for (
      let turn = 0;
      turn < 10 && store.preparation.latency !== "checking";
      turn++
    )
      await new Promise((resolve) => setTimeout(resolve, 0));
    expect(store.preparation.throughput).toBe("disabled");
    expect(store.preparation.latency).toBe("checking");
    cancelPendingStart();
    store.config = previousConfig;
    restoreFetch();
    teardownRunner();
    restoreEnvironment();
  } finally {
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("transfer-only preparation keeps throughput checking and latency disabled", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  try {
    const { bootRunner, cancelPendingStart, teardownRunner, toggleRun } =
      await import("./engine.svelte");
    const { store } = await import("../state/store.svelte");
    const restoreEnvironment = stubBootEnvironment("visible");
    await bootRunner();
    const restoreFetch = stubGlobal(
      "fetch",
      (_input: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>(() => {
          void init;
        }),
    );
    const previousConfig = JSON.parse(JSON.stringify(store.config));
    store.config.stages = {
      latency: false,
      download: true,
      upload: false,
      bidirectional: false,
    };
    store.config.skipLoadedLatencyWhenStageOff = true;
    store.reset();
    toggleRun();
    for (
      let turn = 0;
      turn < 10 && store.preparation.throughput !== "checking";
      turn++
    )
      await new Promise((resolve) => setTimeout(resolve, 0));
    expect(store.preparation.throughput).toBe("checking");
    expect(store.preparation.latency).toBe("disabled");
    cancelPendingStart();
    store.config = previousConfig;
    restoreFetch();
    teardownRunner();
    restoreEnvironment();
  } finally {
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

// A tab opened in the background never fires visibilitychange, so without a seed
// at boot the runner keeps its default "foreground" flag and the keepalive stays
// up in a hidden tab, where Chromium throttles its worker timers past the
// server's idle bound and the connectivity pill latches offline.
test("bootRunner seeds background activity from the live visibilityState", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  const realSetBackground = RunnerCore.prototype.setBackgroundActivity;
  const seeded: boolean[] = [];
  RunnerCore.prototype.setBackgroundActivity = function (enabled: boolean) {
    seeded.push(enabled);
  };
  // Imported with `window` absent, so neither top-level `$effect.root` block runs.
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  try {
    const { bootRunner, teardownRunner } = await import("./engine.svelte");

    let restore = stubBootEnvironment("hidden");
    await bootRunner();
    teardownRunner();
    restore();
    expect(seeded).toEqual([false]);

    restore = stubBootEnvironment("visible");
    await bootRunner();
    teardownRunner();
    restore();
    expect(seeded).toEqual([false, true]);
  } finally {
    RunnerCore.prototype.setBackgroundActivity = realSetBackground;
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("connectivity validation coalesces offline edges and recovers online", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  const { RealBackend } = await import("./RealRunner");
  const originalProbe = RealBackend.prototype.probe;
  let probeCalls = 0;
  let offline = false;
  let releaseOffline: (() => void) | undefined;
  RealBackend.prototype.probe = async function () {
    probeCalls++;
    if (offline) {
      await new Promise<void>((resolve) => {
        releaseOffline = () => resolve();
      });
      throw new Error("server unavailable");
    }
    return PROBE_EVIDENCE;
  };
  const environment = stubEventBootEnvironment("visible", true);
  try {
    const { bootRunner, getRunner, teardownRunner } =
      await import("./engine.svelte");
    const { store } = await import("../state/store.svelte");
    await bootRunner();
    expect(probeCalls).toBe(1);
    expect(store.connectionValidation.throughput.state).toBe("verified");
    expect(store.connectionValidation.latency.state).toBe("verified");

    offline = true;
    const runner = getRunner() as RunnerCore;
    runner.emit({ type: "connectivity", state: "offline" });
    for (let turn = 0; turn < 10 && probeCalls < 2; turn++)
      await new Promise((resolve) => setTimeout(resolve, 0));
    runner.emit({ type: "connectivity", state: "offline" });
    releaseOffline?.();
    await settleValidation();
    expect(probeCalls).toBe(2);
    expect(store.connectionValidation.throughput.state).toBe("failed");
    expect(store.connectionValidation.latency.state).toBe("failed");

    offline = false;
    runner.emit({ type: "connectivity", state: "connected" });
    await settleValidation();
    expect(probeCalls).toBe(3);
    expect(store.connectionValidation.throughput.state).toBe("verified");
    expect(store.connectionValidation.latency.state).toBe("verified");
    teardownRunner();
  } finally {
    RealBackend.prototype.probe = originalProbe;
    environment.restore();
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("validation scheduler refreshes, backs off, defers hidden work, and tears down", async () => {
  Object.assign(globalThis as typeof globalThis & Record<string, unknown>, {
    ...BUILD_TOKENS,
  });
  const restoreWindow = stubGlobal("window", undefined);
  Reflect.deleteProperty(globalThis, "window");
  const { RealBackend } = await import("./RealRunner");
  const originalProbe = RealBackend.prototype.probe;
  let probeCalls = 0;
  let offline = false;
  RealBackend.prototype.probe = async function () {
    probeCalls++;
    if (offline) throw new Error("server unavailable");
    return PROBE_EVIDENCE;
  };
  const environment = stubEventBootEnvironment("visible", true);
  const timers = stubValidationTimers();
  try {
    const { bootRunner, getRunner, teardownRunner } =
      await import("./engine.svelte");
    await bootRunner();
    expect(probeCalls).toBe(1);
    expect(timers.delays()).toContain(CONNECTION_FRESH_MS);

    timers.advance(CONNECTION_FRESH_MS);
    await settleMicrotasks();
    expect(probeCalls).toBe(2);

    offline = true;
    (getRunner() as RunnerCore).emit({
      type: "connectivity",
      state: "offline",
    });
    timers.advance(0);
    await settleMicrotasks();
    expect(probeCalls).toBe(3);
    expect(timers.delays()).toContain(CONNECTION_FRESH_MS);

    timers.advance(CONNECTION_FRESH_MS - 1);
    await settleMicrotasks();
    expect(probeCalls).toBe(3);
    offline = false;
    timers.advance(1);
    await settleMicrotasks();
    expect(probeCalls).toBe(4);

    environment.setVisibility("hidden");
    expect(timers.size()).toBe(0);
    timers.advance(CONNECTION_FRESH_MS * 2);
    environment.setVisibility("visible");
    expect(probeCalls).toBe(4);
    timers.advance(0);
    await settleMicrotasks();
    expect(probeCalls).toBe(5);

    const runner = getRunner() as RunnerCore;
    runner.emit({
      type: "phase",
      transition: { from: "idle", to: "download", stage: "download", t: 0 },
    });
    timers.advance(CONNECTION_FRESH_MS * 2);
    await settleMicrotasks();
    expect(probeCalls).toBe(5);
    runner.emit({
      type: "phase",
      transition: {
        from: "download",
        to: "complete",
        stage: null,
        t: CONNECTION_FRESH_MS * 2,
      },
    });
    timers.advance(0);
    await settleMicrotasks();
    expect(probeCalls).toBe(6);

    teardownRunner();
    expect(timers.size()).toBe(0);
  } finally {
    RealBackend.prototype.probe = originalProbe;
    timers.restore();
    environment.restore();
    restoreWindow();
    for (const key of Object.keys(BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  }
});

test("a rerun stamps a fresh start epoch from every terminal phase", async () => {
  const { store } = await import("../state/store.svelte");
  for (const from of ["complete", "aborted", "error"] as const) {
    store.reset();
    expect(store.startEpoch).toBe(0);
    store.ingest({
      type: "phase",
      transition: { from, to: "connecting", stage: null, t: 0 },
    });
    expect(store.startEpoch).toBeGreaterThan(0);
  }
});
