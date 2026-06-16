/* ============================================================
 * The Graphite Meter — Runner Wiring Helper (§6)
 * The single integration seam. Swapping DummyRunner for a real
 * engine touches ONLY this file.
 *
 * Named `.svelte.ts` so the `$state.snapshot` rune (used to pass
 * a frozen, non-reactive config into the engine) is compiled.
 * ============================================================ */

import type { NetworkRunner } from "./contract";
import { DummyRunner } from "./dummy";
import { console as store } from "../state/console.svelte";

let runner: NetworkRunner | null = null;
let unsub: (() => void) | null = null;

export function getRunner(): NetworkRunner {
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

export function teardownRunner() {
  unsub?.();
  unsub = null;
}
