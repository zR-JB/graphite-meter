// engine.svelte.ts is a rune module: bun runs TypeScript but not the Svelte
// compiler, so the module is compiled on load here. It is imported with `window`
// absent so neither its own nor store.svelte.ts's top-level `$effect.root` block
// runs; bootRunner() needs no reactive root, only the DOM seams it touches.
import { test, expect } from "bun:test";
import { plugin, Transpiler } from "bun";
import { compileModule } from "svelte/compiler";
import { RunnerCore } from "./core";
import type { InfraInfo } from "./contract";

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
  store.reset();
  expect(store.startError).toBe("");
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
