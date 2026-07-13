/* ============================================================
 * Runner Wiring Helper — integration seam, backend swap
 * The single integration seam. The shared RunnerCore owns the
 * engine logic; only the backend (sample source) is swapped here.
 * Going live touches ONLY this file.
 *
 * Named `.svelte.ts` so the `$state.snapshot` rune (used to pass
 * a frozen, non-reactive config into the engine) is compiled.
 * ============================================================ */

import type { NetworkRunner, RunnerAnomaly } from "./contract";
import { RunnerCore } from "./core";
import { adaptiveWarmupMs } from "./schedule";
// NOTE: DummyBackend is referenced only inside the `__GM_ALLOW_DUMMY__`-guarded
// branch in getRunner(). When that token folds to `false` (a prod build with
// GM_CLIENT_ALLOW_DUMMY=0), Rollup deletes the branch, this import becomes
// unused, and — because dummy.ts is side-effect-free — the whole module is
// tree-shaken out. Keep dummy.ts free of top-level side effects or it'll stay.
import { DummyBackend } from "./dummy";
import { RealBackend, TransportUnavailableError } from "./RealRunner";
import { store } from "../state/store.svelte";
import { setDebugLogging } from "../debug";
import { BUILD } from "../buildenv";

let runner: NetworkRunner | null = null;
let unsub: (() => void) | null = null;

// Mirror the persisted dev toggle into the (main-thread) debug logger, live.
// Workers are separate module graphs — they're told the value in their `start`
// message (RealRunner reads debugEnabled() at spawn), so this only governs the
// main-thread core/RealRunner logs.
if (typeof window !== "undefined") {
  $effect.root(() => {
    $effect(() => setDebugLogging(store.debugLogging));
  });
}

/** Which sample source the app is wired to. `dummy` synthesizes samples (the
 *  default, so the app works with no server); `real` talks to the live Go
 *  backend. The real engine is built out stage by stage — Stage 1 implements
 *  only `probe()`, so a `real` run currently lights up the infra panel but
 *  cannot Engage yet. */
type EngineKind = "dummy" | "real";

const ENGINE_STORAGE_KEY = "gm.engine";

/** Resolve the engine: a `?engine=real|dummy` URL param wins (and is persisted
 *  for subsequent reloads), else the last persisted choice, else the build's
 *  configured default (`GM_CLIENT_ENGINE`, "real" unless overridden). */
function resolveEngine(): EngineKind {
  if (typeof window !== "undefined") {
    const param = new URLSearchParams(window.location.search).get("engine");
    if (param === "real" || param === "dummy") {
      try {
        window.localStorage.setItem(ENGINE_STORAGE_KEY, param);
      } catch {
        /* private mode / storage disabled — fall through to the param value */
      }
      return param;
    }
    try {
      const saved = window.localStorage.getItem(ENGINE_STORAGE_KEY);
      if (saved === "real" || saved === "dummy") return saved;
    } catch {
      /* storage unavailable — use the default */
    }
  }
  return BUILD.defaultEngine;
}

export function getRunner(): NetworkRunner {
  // The single integration seam. The core/UI/store are engine-agnostic; only
  // the backend (sample source) is selected here.
  //
  // The dummy branch is gated on the raw `__GM_ALLOW_DUMMY__` literal so that a
  // prod build (GM_CLIENT_ALLOW_DUMMY=0) folds it away and tree-shakes the
  // DummyBackend out — a real-only bundle then ignores any persisted/`?engine=`
  // "dummy" and always runs the real backend.
  if (!runner) {
    if (__GM_ALLOW_DUMMY__ && resolveEngine() === "dummy") {
      runner = new RunnerCore(new DummyBackend({ profile: "fiber" }));
    } else {
      runner = new RunnerCore(
        new RealBackend({ endpoint: store.config.endpoint }),
      );
    }
  }
  return runner;
}

/** Call once on app mount. Probes infra, subscribes store to events. */
export async function bootRunner() {
  const r = getRunner();
  store.engineInfo = r.describe();
  unsub = r.on((e) => store.ingest(e));
  try {
    const info = await r.probe(store.config.endpoint);
    store.ingest({ type: "infra", info });
  } catch (cause) {
    store.ingest({
      type: "error",
      error: {
        reason:
          cause instanceof TransportUnavailableError
            ? "transport-unavailable"
            : "preflight-failed",
        message: "Probe failed",
        phase: "idle",
        cause,
      },
    });
  }
}

export function engage() {
  if (store.isRunning) {
    getRunner().abort();
    return;
  }
  store.reset();
  // Stretch warmup to the measured RTT so slow-start finishes before measuring
  // (the user's configured warmup stays the floor). Mutates the throwaway snapshot
  // only — the persisted setting is untouched.
  const cfg = $state.snapshot(store.config);
  cfg.duration = {
    ...cfg.duration,
    warmupMs: adaptiveWarmupMs(
      cfg.duration.warmupMs,
      store.infra?.preTestPingMs ?? 0,
    ),
  };
  // Resolve the selected target before every run. RealBackend caches logical
  // discovery, so this performs one selected /probe without another preflight.
  getRunner()
    .probe(cfg.endpoint)
    .then((info) => {
      store.ingest({ type: "infra", info });
      getRunner().start(cfg);
    })
    .catch((cause) => {
      store.ingest({
        type: "error",
        error: {
          reason:
            cause instanceof TransportUnavailableError
              ? "transport-unavailable"
              : "preflight-failed",
          message: "Couldn't reach the server",
          phase: "idle",
          cause,
        },
      });
    });
}

/**
 * Return to the fresh, blank idle view — what the logo click offers, matching
 * a page reload's starting state (persisted settings are untouched). Aborts any
 * in-flight run first (synchronous in the dummy engine, so the subsequent reset
 * sticks), then clears samples / result / phase back to idle.
 */
export function returnToStart() {
  if (store.isRunning) getRunner().abort();
  store.reset();
}

/**
 * Push the live enabled-stage set into the running engine so a mid-run
 * future-stage toggle actually shortens the run. No-op when idle or
 * when the active engine doesn't support live reconfigure.
 */
export function applyStageChange() {
  if (!store.isRunning) return;
  getRunner().reconfigureStages?.($state.snapshot(store.config.stages));
}

/**
 * Pass a live dev anomaly through to the active engine. Optional on
 * the contract, so this is a no-op when the engine doesn't implement it or
 * when nothing is running (the runner itself guards on its tick timer).
 */
export function injectAnomaly(a: RunnerAnomaly) {
  getRunner().injectAnomaly?.(a);
}

export function teardownRunner() {
  unsub?.();
  unsub = null;
}
