/* ============================================================
 * Runner Wiring Helper — integration seam, backend swap
 * The single integration seam. The shared RunnerCore owns the
 * engine logic; only the backend (sample source) is swapped here.
 * Going live touches ONLY this file.
 *
 * Named `.svelte.ts` so the `$state.snapshot` rune (used to pass
 * a frozen, non-reactive config into the engine) is compiled.
 * ============================================================ */

import type {
  ConnectionRole,
  InfraInfo,
  LiveRunConfig,
  NetworkRunner,
  RunnerAnomaly,
  RunnerEvent,
} from "./contract";
import { RunnerCore } from "./core";
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
import {
  CONNECTION_FRESH_MS,
  CONNECTION_ROLES,
  connectionKey,
  connectionRoleKey,
  connectionSelection,
  validationRoles,
} from "./connectionModel";

let runner: NetworkRunner | null = null;
let unsub: (() => void) | null = null;
let validationAbort: AbortController | null = null;
let validationSeq = 0;
let booted = false;
let prepared: { key: string; info: InfraInfo; verifiedAt: number } | null =
  null;
let lastRoleKeys: Record<ConnectionRole, string> = {
  throughput: "",
  latency: "",
};
let pendingValidation = false;
let hiddenAt = 0;
let validating: ConnectionRole[] = [];

// Mirror the persisted dev toggle into the (main-thread) debug logger, live.
// Workers are separate module graphs — they're told the value in their `start`
// message (RealRunner reads debugEnabled() at spawn), so this only governs the
// main-thread core/RealRunner logs.
if (typeof window !== "undefined") {
  $effect.root(() => {
    $effect(() => setDebugLogging(store.debugLogging));
    $effect(() => {
      const changed = CONNECTION_ROLES.filter(
        (role) => connectionRoleKey(store.config, role) !== lastRoleKeys[role],
      );
      const running = store.isRunning;
      if (!booted) return;
      if (changed.length) {
        if (changed.includes("latency")) store.idleLatency = [];
        for (const role of changed)
          lastRoleKeys[role] = connectionRoleKey(store.config, role);
        pendingValidation = true;
        if (running) {
          markValidation(
            changed,
            "stale",
            "Draft changed; validation resumes after this run.",
          );
          return;
        }
      }
      if (running || !pendingValidation) return;
      pendingValidation = false;
      queueMicrotask(
        () =>
          void validateConnections(
            false,
            changed.length === 1 ? changed[0] : undefined,
          ).catch(() => {}),
      );
    });
  });
}

/** Which sample source the app is wired to. `dummy` synthesizes samples for
 *  development; `real` talks to the live Go backend. */
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
      runner = new RunnerCore(new RealBackend());
    }
  }
  return runner;
}

function validationMessage(cause: unknown): string {
  if (cause instanceof TransportUnavailableError) return cause.message;
  return cause instanceof Error ? cause.message : "Connection check failed";
}

function markValidation(
  roles: ConnectionRole[],
  state: "checking" | "verified" | "failed" | "stale",
  message?: string,
  verifiedAt?: number,
) {
  const next = { ...store.connectionValidation };
  for (const role of roles)
    next[role] = {
      selection: connectionSelection(store.config, role),
      state,
      message,
      verifiedAt,
    };
  store.connectionValidation = next;
}

function preparedIsFresh(key: string): boolean {
  return !!(
    prepared &&
    prepared.key === key &&
    Date.now() - prepared.verifiedAt <= CONNECTION_FRESH_MS &&
    prepared.info.discoveryGeneration === store.transportDiscovery?.generation
  );
}

export async function validateConnections(
  force = false,
  requestedRole?: ConnectionRole,
): Promise<InfraInfo> {
  const key = connectionKey(store.config);
  if (!force && preparedIsFresh(key)) return prepared!.info;
  if (validating.length) markValidation(validating, "stale");
  validationAbort?.abort();
  const roles = validationRoles(
    store.config,
    store.connectionValidation,
    requestedRole,
  );
  const abort = new AbortController();
  validationAbort = abort;
  const seq = ++validationSeq;
  validating = roles;
  markValidation(roles, "checking");
  const generation = store.transportDiscovery?.generation;
  try {
    const info = await getRunner().probe(
      $state.snapshot(store.config),
      abort.signal,
      roles.length === 1 ? roles[0] : undefined,
    );
    if (abort.signal.aborted || seq !== validationSeq)
      throw new DOMException("Aborted", "AbortError");
    const verifiedAt = Date.now();
    const verifiedRoles =
      generation && generation !== info.discoveryGeneration
        ? CONNECTION_ROLES
        : roles;
    prepared = { key, info, verifiedAt };
    store.ingest({ type: "infra", info });
    markValidation(verifiedRoles, "verified", undefined, verifiedAt);
    return info;
  } catch (cause) {
    if (abort.signal.aborted || seq !== validationSeq) throw cause;
    prepared = null;
    markValidation(roles, "failed", validationMessage(cause));
    throw cause;
  } finally {
    if (validationAbort === abort) {
      validationAbort = null;
      validating = [];
    }
  }
}

function ingestRunnerEvent(event: RunnerEvent) {
  if (
    event.type === "error" &&
    [
      "connection-lost",
      "timeout",
      "preflight-failed",
      "transport-unavailable",
    ].includes(event.error.reason)
  ) {
    prepared = null;
    markValidation(
      CONNECTION_ROLES,
      "stale",
      "Connection changed; check again.",
    );
  }
  store.ingest(event);
}

function refreshAfterTransition() {
  if (!store.isRunning) void validateConnections(true).catch(() => {});
}

function refreshAfterVisibility() {
  if (document.visibilityState === "hidden") {
    hiddenAt = Date.now();
  } else if (hiddenAt && Date.now() - hiddenAt >= CONNECTION_FRESH_MS) {
    hiddenAt = 0;
    refreshAfterTransition();
  }
}

export async function bootRunner() {
  const r = getRunner();
  store.engineInfo = r.describe();
  unsub = r.on(ingestRunnerEvent);
  booted = true;
  for (const role of CONNECTION_ROLES)
    lastRoleKeys[role] = connectionRoleKey(store.config, role);
  window.addEventListener("online", refreshAfterTransition);
  document.addEventListener("visibilitychange", refreshAfterVisibility);
  await validateConnections().catch(() => {});
}

export function engage() {
  if (store.isRunning) {
    getRunner().abort();
    return;
  }
  store.reset();
  const cfg = $state.snapshot(store.config);
  const key = connectionKey(cfg);
  const start = async () => {
    const info = preparedIsFresh(key)
      ? prepared!.info
      : await validateConnections();
    if (!preparedIsFresh(key)) return;
    store.activeConfig = structuredClone(cfg);
    store.activeConnections = $state.snapshot(store.connections);
    await getRunner().start(cfg, info);
  };
  start().catch((cause) => {
    // An abort invalidates the pending start and resolves it without error.
    if (store.phase === "aborted") return;
    store.ingest({
      type: "error",
      error: {
        reason:
          cause instanceof TransportUnavailableError
            ? "transport-unavailable"
            : "preflight-failed",
        message: "Couldn't reach the server",
        phase: "connecting",
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
  void validateConnections(true).catch(() => {});
}

/**
 * Push the live enabled-stage set into the running engine so a mid-run
 * future-stage toggle actually shortens the run. No-op when idle or
 * when the active engine doesn't support live reconfigure.
 */
export function applyLiveRunConfig() {
  if (!store.isRunning) return;
  const config = $state.snapshot(store.config);
  const live: LiveRunConfig = {
    stages: config.stages,
    duration: config.duration,
    adaptive: config.adaptive,
  };
  getRunner().reconfigure?.(live);
  if (store.activeConfig)
    store.activeConfig = { ...store.activeConfig, ...live };
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
  booted = false;
  validationAbort?.abort();
  validationAbort = null;
  window.removeEventListener("online", refreshAfterTransition);
  document.removeEventListener("visibilitychange", refreshAfterVisibility);
  unsub?.();
  unsub = null;
}
