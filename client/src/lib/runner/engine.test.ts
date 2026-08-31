import { test, expect } from "bun:test";
import { plugin, Transpiler } from "bun";
import { compileModule } from "svelte/compiler";
import { RunnerCore } from "./core";
import type { InfraInfo, RunnerConfig } from "./contract";
import {
  PreflightUnavailableError,
  TransportUnavailableError,
} from "./real/transportError";
import {
  CONNECTION_FAILURE_BACKOFF_MS,
  CONNECTION_FRESH_MS,
} from "./connectionModel";
import { TEST_BUILD_TOKENS } from "./test-helpers.test";
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
function stubBuildGlobals(): () => void {
  Object.assign(globalThis as Record<string, unknown>, TEST_BUILD_TOKENS);
  return () => {
    for (const key of Object.keys(TEST_BUILD_TOKENS))
      Reflect.deleteProperty(globalThis, key);
  };
}
function stubEngineGlobals(): () => void {
  const restoreBuild = stubBuildGlobals();
  const restoreWindow = stubGlobal("window", undefined);
  return () => {
    restoreWindow();
    restoreBuild();
  };
}
async function yieldUntil(done: () => boolean, turns = 10): Promise<void> {
  for (let turn = 0; turn < turns && !done(); turn++)
    await new Promise((resolve) => setTimeout(resolve, 0));
}
async function withBootRunner(
  run: (engine: typeof import("./engine.svelte")) => Promise<void>,
  setup: () => () => void = () => stubBootEnvironment("visible"),
): Promise<void> {
  const restoreGlobals = stubEngineGlobals();
  const engine = await import("./engine.svelte");
  const restoreEnvironment = setup();
  try {
    await engine.bootRunner();
    await run(engine);
  } finally {
    engine.teardownRunner();
    restoreEnvironment();
    restoreGlobals();
  }
}
async function checkPreparation(
  stages: RunnerConfig["stages"],
  checked: "throughput" | "latency",
): Promise<void> {
  await withBootRunner(async ({ cancelPendingStart, toggleRun }) => {
    const { store } = await import("../state/store.svelte");
    const restoreFetch = stubGlobal(
      "fetch",
      (_input: RequestInfo | URL, init?: RequestInit) =>
        new Promise<Response>(() => void init),
    );
    const previousConfig = JSON.parse(JSON.stringify(store.config));
    store.config.stages = stages;
    store.config.skipLoadedLatencyWhenStageOff = true;
    store.reset();
    toggleRun();
    await yieldUntil(() => store.preparation[checked] === "checking");
    const other = checked === "throughput" ? "latency" : "throughput";
    expect(store.preparation[checked]).toBe("checking");
    expect(store.preparation[other]).toBe("disabled");
    cancelPendingStart();
    store.config = previousConfig;
    restoreFetch();
  });
}
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
    stubGlobal("fetch", () => Promise.reject(new Error("no network"))),
  ];
  return () => {
    for (const restore of restores.reverse()) restore();
  };
}
function eventTarget() {
  const listeners = new Map<string, () => void>();
  return {
    addEventListener(type: string, listener: () => void) {
      listeners.set(type, listener);
    },
    removeEventListener(type: string) {
      listeners.delete(type);
    },
    emit(type: string) {
      listeners.get(type)?.();
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
    addEventListener: documentListeners.addEventListener,
    removeEventListener: documentListeners.removeEventListener,
  };
  const windowValue = {
    addEventListener: windowListeners.addEventListener,
    removeEventListener: windowListeners.removeEventListener,
  };
  const restores = [
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
type ValidationContext = {
  engine: typeof import("./engine.svelte");
  environment: ReturnType<typeof stubEventBootEnvironment>;
  probeCalls: () => number;
};
async function withValidationRunner(
  probe: () => Promise<InfraInfo>,
  run: (context: ValidationContext) => Promise<void>,
): Promise<void> {
  const restoreGlobals = stubEngineGlobals();
  const { RealBackend } = await import("./RealRunner");
  const originalProbe = RealBackend.prototype.probe;
  let calls = 0;
  RealBackend.prototype.probe = async function () {
    calls++;
    return probe();
  };
  const environment = stubEventBootEnvironment("visible", true);
  const engine = await import("./engine.svelte");
  try {
    await engine.bootRunner();
    await run({ engine, environment, probeCalls: () => calls });
  } finally {
    engine.teardownRunner();
    RealBackend.prototype.probe = originalProbe;
    environment.restore();
    restoreGlobals();
  }
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
test("teardown clears the probe evidence; a run reset keeps it", async () => {
  await withBootRunner(async ({ teardownRunner }) => {
    const { store } = await import("../state/store.svelte");
    store.ingest({ type: "infra", info: PROBE_EVIDENCE });
    store.reset();
    expect(store.infra).toEqual(PROBE_EVIDENCE);
    teardownRunner();
    expect(store.infra).toBeNull();
  });
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
test("runner boundary canonicalizes every adaptive tuning field", async () => {
  const { canonicalAdaptiveConfig, DEFAULT_CONFIG } =
    await import("../state/defaults");
  const hostile = {
    ...DEFAULT_CONFIG.adaptive,
    enabled: false,
    minCoverageRatio: 0.01,
    stabilityThreshold: 0.01,
    maxPhaseReductionRatio: 0.99,
    minLatencySamples: 1,
    minTransferSamples: 1,
    confirmationMs: 1,
    glideMs: 1,
  };
  expect(canonicalAdaptiveConfig(hostile)).toEqual({
    ...DEFAULT_CONFIG.adaptive,
    enabled: false,
  });
});
test("preparation names a disabled throughput path for latency-only runs", async () => {
  await checkPreparation(
    {
      latency: true,
      download: false,
      upload: false,
      bidirectional: false,
    },
    "latency",
  );
});
test("transfer-only preparation keeps throughput checking and latency disabled", async () => {
  await checkPreparation(
    {
      latency: false,
      download: true,
      upload: false,
      bidirectional: false,
    },
    "throughput",
  );
});
test("bootRunner seeds background activity from the live visibilityState", async () => {
  const restoreGlobals = stubEngineGlobals();
  const realSetBackground = RunnerCore.prototype.setBackgroundActivity;
  const seeded: boolean[] = [];
  RunnerCore.prototype.setBackgroundActivity = function (enabled: boolean) {
    seeded.push(enabled);
  };
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
    restoreGlobals();
  }
});
test("connectivity validation coalesces offline edges and recovers online", async () => {
  let offline = false;
  let releaseOffline: (() => void) | undefined;
  await withValidationRunner(
    async () => {
      if (offline) {
        await new Promise<void>((resolve) => {
          releaseOffline = () => resolve();
        });
        throw new Error("server unavailable");
      }
      return PROBE_EVIDENCE;
    },
    async ({ engine, probeCalls }) => {
      const { getRunner } = engine;
      const { store } = await import("../state/store.svelte");
      expect(probeCalls()).toBe(1);
      expect(store.connectionValidation.throughput.state).toBe("verified");
      expect(store.connectionValidation.latency.state).toBe("verified");
      offline = true;
      const runner = getRunner() as RunnerCore;
      runner.emit({ type: "connectivity", state: "offline" });
      await yieldUntil(() => probeCalls() >= 2);
      runner.emit({ type: "connectivity", state: "offline" });
      releaseOffline?.();
      await settleValidation();
      expect(probeCalls()).toBe(2);
      expect(store.connectionValidation.throughput.state).toBe("failed");
      expect(store.connectionValidation.latency.state).toBe("failed");
      offline = false;
      runner.emit({ type: "connectivity", state: "connected" });
      await settleValidation();
      expect(probeCalls()).toBe(3);
      expect(store.connectionValidation.throughput.state).toBe("verified");
      expect(store.connectionValidation.latency.state).toBe("verified");
    },
  );
});
test("window connectivity listeners share failure and recovery scheduling", async () => {
  let offline = false;
  await withValidationRunner(
    async () => {
      if (offline) throw new Error("server unavailable");
      return PROBE_EVIDENCE;
    },
    async ({ engine, environment, probeCalls }) => {
      const { store } = await import("../state/store.svelte");
      offline = true;
      environment.emit("offline");
      environment.emit("offline");
      await settleValidation();
      expect(probeCalls()).toBe(2);
      expect(store.connectionValidation.throughput.state).toBe("failed");
      offline = false;
      environment.emit("online");
      await settleValidation();
      expect(probeCalls()).toBe(3);
      expect(store.connectionValidation.throughput.state).toBe("verified");
      void engine;
    },
  );
});

test("a path-specific validation failure leaves global keepalive state unchanged", async () => {
  let failThroughput = false;
  await withValidationRunner(
    async () => {
      if (failThroughput)
        throw new TransportUnavailableError("throughput unavailable", {
          role: "throughput",
        });
      return PROBE_EVIDENCE;
    },
    async ({ engine }) => {
      const { store } = await import("../state/store.svelte");
      failThroughput = true;
      await expect(
        engine.validateConnections(true, "throughput"),
      ).rejects.toThrow("throughput unavailable");
      expect(store.connectionValidation.throughput.state).toBe("failed");
      expect(store.connectivity).toBe("connected");
    },
  );
});

test("validation scheduler refreshes, backs off, defers hidden work, and tears down", async () => {
  let offline = false;
  const timers = stubValidationTimers();
  try {
    await withValidationRunner(
      async () => {
        if (offline) throw new Error("server unavailable");
        return PROBE_EVIDENCE;
      },
      async ({ engine, environment, probeCalls }) => {
        const { getRunner } = engine;
        expect(probeCalls()).toBe(1);
        expect(timers.delays()).toContain(CONNECTION_FRESH_MS);
        timers.advance(CONNECTION_FRESH_MS);
        await settleMicrotasks();
        expect(probeCalls()).toBe(2);
        offline = true;
        (getRunner() as RunnerCore).emit({
          type: "connectivity",
          state: "offline",
        });
        timers.advance(0);
        await settleMicrotasks();
        expect(probeCalls()).toBe(3);
        expect(timers.delays()).toContain(CONNECTION_FAILURE_BACKOFF_MS[0]);
        timers.advance(CONNECTION_FAILURE_BACKOFF_MS[0] - 1);
        await settleMicrotasks();
        expect(probeCalls()).toBe(3);
        offline = false;
        timers.advance(1);
        await settleMicrotasks();
        expect(probeCalls()).toBe(4);
        timers.advance(CONNECTION_FRESH_MS - 1);
        await settleMicrotasks();
        expect(probeCalls()).toBe(4);
        environment.setVisibility("hidden");
        expect(timers.size()).toBe(0);
        timers.advance(2);
        environment.setVisibility("visible");
        expect(probeCalls()).toBe(4);
        timers.advance(0);
        await settleMicrotasks();
        expect(probeCalls()).toBe(5);
        const runner = getRunner() as RunnerCore;
        runner.emit({
          type: "phase",
          transition: { from: "idle", to: "download", stage: "download", t: 0 },
        });
        timers.advance(CONNECTION_FRESH_MS * 2);
        await settleMicrotasks();
        expect(probeCalls()).toBe(5);
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
        expect(probeCalls()).toBe(6);
        engine.teardownRunner();
        expect(timers.size()).toBe(0);
      },
    );
  } finally {
    timers.restore();
  }
});

test("validation failures use staged backoff, cap, and reset after recovery", async () => {
  let available = false;
  const timers = stubValidationTimers();
  try {
    await withValidationRunner(
      async () => {
        if (!available) throw new Error("server unavailable");
        return PROBE_EVIDENCE;
      },
      async ({ probeCalls }) => {
        const maxRetryDelay =
          CONNECTION_FAILURE_BACKOFF_MS[
            CONNECTION_FAILURE_BACKOFF_MS.length - 1
          ];
        expect(probeCalls()).toBe(1);
        for (const delay of CONNECTION_FAILURE_BACKOFF_MS) {
          expect(timers.delays()).toContain(delay);
          timers.advance(delay);
          await settleMicrotasks();
          expect(probeCalls()).toBeGreaterThanOrEqual(2);
        }
        expect(timers.delays()).toContain(maxRetryDelay);
        available = true;
        timers.advance(maxRetryDelay);
        await settleMicrotasks();
        expect(probeCalls()).toBe(7);
        expect(timers.delays()).toContain(CONNECTION_FRESH_MS);
      },
    );
  } finally {
    timers.restore();
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
