/* ============================================================
 * The Graphite Meter — Runner Wiring Helper (§6)
 * The single integration seam. Swapping DummyRunner for a real
 * engine touches ONLY this file.
 *
 * Named `.svelte.ts` so the `$state.snapshot` rune (used to pass
 * a frozen, non-reactive config into the engine) is compiled.
 * ============================================================ */

import type { NetworkRunner, RunnerAnomaly } from "./contract";
import { DummyRunner } from "./dummy";
import { console as store } from "../state/console.svelte";

let runner: NetworkRunner | null = null;
let unsub: (() => void) | null = null;

export function getRunner(): NetworkRunner {
  // To go live against a real speedtest backend, change this ONE line — the UI
  // and store are engine-agnostic (§14.4 · see docs/REAL_RUNNER.md):
  //   import { RealRunner } from "./RealRunner";
  //   if (!runner) runner = new RealRunner({ endpoint: store.config.endpoint });
  if (!runner) runner = new DummyRunner({ profile: "fiber" });
  return runner;
}

/** Call once on app mount. Probes infra, subscribes store to events. */
export async function bootRunner() {
  const r = getRunner();
  unsub = r.on((e) => store.ingest(e));
  try {
    const info = await r.probe(store.config.endpoint);
    store.ingest({ type: "infra", info });
  } catch {
    store.ingest({ type: "error", message: "Probe failed" });
  }
}

export function engage() {
  if (store.isRunning) {
    getRunner().abort();
    return;
  }
  store.reset();
  getRunner().start($state.snapshot(store.config));
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
 * future-stage toggle actually shortens the run (§13.4). No-op when idle or
 * when the active engine doesn't support live reconfigure.
 */
export function applyStageChange() {
  if (!store.isRunning) return;
  const r = getRunner() as NetworkRunner & {
    reconfigureStages?: (s: typeof store.config.stages) => void;
  };
  r.reconfigureStages?.($state.snapshot(store.config.stages));
}

/**
 * Pass a live dev anomaly through to the active engine (§13.6). Optional on
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
