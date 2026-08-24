/* ============================================================
 * Runner engine wiring: the integration seam for a backend swap
 * The single integration seam. The shared RunnerCore owns the
 * engine logic; only the backend (sample source) is swapped here.
 * Going live touches ONLY this file.
 *
 * Named `.svelte.ts` so the `$state.snapshot` rune compiles; it
 * passes a frozen, non-reactive config into the engine.
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
// DummyBackend is referenced only inside the `__GM_ALLOW_DUMMY__` branch in
// getRunner(). With that token folded to `false` (GM_CLIENT_ALLOW_DUMMY=0),
// Rollup deletes the branch and tree-shakes the whole module out, but only
// while dummy.ts stays free of top-level side effects.
import { DummyBackend } from "./dummy";
import { RealBackend, TransportUnavailableError } from "./RealRunner";
import { store } from "../state/store.svelte";
import { setDebugLogging } from "../debug";
import { BUILD } from "../buildenv";
import {
  requireSessionCoverage,
  liveScheduleFitsSession,
  SessionCoverageError,
  type SessionBudget,
} from "../auth";
import { buildSegments } from "./schedule";
import {
  CONNECTION_FAILURE_REASONS,
  CONNECTION_FRESH_MS,
  CONNECTION_ROLES,
  type ConnectionValidationState,
  connectionDraftKey,
  connectionDraftRoleKey,
  roleNeedsValidation,
  connectionKey,
  connectionRoleKey,
  connectionSelection,
  validationRoles,
  verifiedRolesForProbe,
} from "./connectionModel";

let runner: NetworkRunner | null = null;
let unsubscribe: (() => void) | null = null;
let validationAbort: AbortController | null = null;
let pendingStartAbort: AbortController | null = null;
let pendingStartSeq = 0;
let validationSeq = 0;
let booted = false;
let prepared: { key: string; info: InfraInfo; verifiedAt: number } | null =
  null;
let lastDraftRoleKeys: Record<ConnectionRole, string> = {
  throughput: "",
  latency: "",
};
let pendingValidation = false;
let hiddenAt = 0;
let validating: ConnectionRole[] = [];
let sessionBudget: SessionBudget | null = null;
const SESSION_RUN_MARGIN_MS = 60_000;

export function cancelPendingStart() {
  if (!pendingStartAbort) {
    store.startPending = false;
    return;
  }
  pendingStartSeq++;
  pendingStartAbort.abort();
  pendingStartAbort = null;
  store.startPending = false;
}

// Mirror the persisted dev toggle into the main-thread debug logger, live.
// Workers are separate module graphs: they get the value in their `start`
// message, so this governs only the main-thread core/RealRunner logs.
if (typeof window !== "undefined") {
  $effect.root(() => {
    $effect(() => setDebugLogging(store.debugLogging));
    $effect(() => {
      // Read every reactive dependency ahead of the `booted` guard: `booted` is
      // a plain variable, so an early return loses the effect's subscriptions.
      const changed = CONNECTION_ROLES.filter(
        (role) =>
          connectionDraftRoleKey(store.config, role) !==
          lastDraftRoleKeys[role],
      );
      // Every role with something to learn from a probe, not only the ones the
      // draft moved: a role left unverified by an earlier failure is still owed one.
      const needed = CONNECTION_ROLES.filter((role) =>
        roleNeedsValidation(
          store.config,
          store.connectionValidation,
          role,
          store.transportDiscovery,
        ),
      );
      const running = store.isRunning;
      if (!booted) return;
      if (changed.length) {
        // Only a latency path that both moved and needs re-checking invalidates
        // its samples. A stage toggle must not blank the sparkline.
        if (changed.includes("latency") && needed.includes("latency"))
          store.idleLatency = [];
        for (const role of changed)
          lastDraftRoleKeys[role] = connectionDraftRoleKey(store.config, role);
        if (!needed.length) return;
        pendingValidation = true;
        if (running) {
          markValidation(
            changed.filter((role) => needed.includes(role)),
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
          // A rejection is already recorded as a "failed" role by markValidation.
          void validateConnections(
            false,
            needed.length === 1 ? needed[0] : undefined,
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
        /* private mode or storage disabled: fall through to the param value */
      }
      return param;
    }
    try {
      const saved = window.localStorage.getItem(ENGINE_STORAGE_KEY);
      if (saved === "real" || saved === "dummy") return saved;
    } catch {
      /* storage unavailable: use the default */
    }
  }
  return BUILD.defaultEngine;
}

export function getRunner(): NetworkRunner {
  // Gated on the raw `__GM_ALLOW_DUMMY__` literal so a prod build folds it away.
  // A real-only bundle then ignores any persisted or `?engine=` "dummy".
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
  state: ConnectionValidationState,
  message?: string,
  verifiedAt?: number,
  config = store.config,
) {
  const next = { ...store.connectionValidation };
  for (const role of roles)
    next[role] = {
      selection: connectionSelection(config, role),
      identity: connectionRoleKey(config, role, store.transportDiscovery),
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
  if (!booted) throw new DOMException("Runner is not active", "AbortError");
  const config = $state.snapshot(store.config);
  const key = connectionKey(config, store.transportDiscovery);
  const draftKey = connectionDraftKey(config);
  if (!force && preparedIsFresh(key)) return prepared!.info;
  if (validating.length) markValidation(validating, "stale");
  validationAbort?.abort();
  const roles = validationRoles(
    config,
    store.connectionValidation,
    requestedRole,
    store.transportDiscovery,
  );
  const abort = new AbortController();
  validationAbort = abort;
  const seq = ++validationSeq;
  validating = roles;
  markValidation(roles, "checking", undefined, undefined, config);
  const transactionCurrent = (): boolean =>
    booted &&
    !abort.signal.aborted &&
    seq === validationSeq &&
    connectionDraftKey($state.snapshot(store.config)) === draftKey;
  try {
    let latest: InfraInfo | null = null;
    let firstFailure: unknown;
    const batches: (ConnectionRole | undefined)[] =
      roles.length === CONNECTION_ROLES.length ? [undefined] : roles;
    for (const role of batches) {
      const batchRoles = role ? [role] : roles;
      const discoveryGeneration = store.transportDiscovery?.generation;
      try {
        const info = await getRunner().probe(config, abort.signal, role);
        if (!transactionCurrent())
          throw new DOMException("Aborted", "AbortError");
        latest = info;
        const verifiedAt = Date.now();
        store.ingest({ type: "infra", info });
        markValidation(
          verifiedRolesForProbe(
            batchRoles,
            discoveryGeneration,
            info.discoveryGeneration,
          ),
          "verified",
          undefined,
          verifiedAt,
          config,
        );
      } catch (cause) {
        if (!transactionCurrent()) throw cause;
        firstFailure ??= cause;
        const failedRoles =
          cause instanceof TransportUnavailableError && cause.role
            ? [cause.role]
            : batchRoles;
        markValidation(
          failedRoles,
          "failed",
          validationMessage(cause),
          undefined,
          config,
        );
        const uncheckedRoles = batchRoles.filter(
          (batchRole) => !failedRoles.includes(batchRole),
        );
        if (uncheckedRoles.length)
          markValidation(
            uncheckedRoles,
            "stale",
            "Not checked because the other path failed.",
            undefined,
            config,
          );
      }
    }
    if (firstFailure) {
      prepared = null;
      throw firstFailure;
    }
    if (!latest) throw new Error("no connection role was validated");
    prepared = {
      key: connectionKey(config, store.transportDiscovery),
      info: latest,
      verifiedAt: Date.now(),
    };
    return latest;
  } finally {
    if (validationAbort === abort) {
      validationAbort = null;
      validating = [];
    }
  }
}

function ingestRunnerEvent(event: RunnerEvent) {
  if (
    event.type === "transportDiscovery" &&
    store.transportDiscovery &&
    event.discovery.generation !== store.transportDiscovery.generation
  ) {
    store.ingest(event);
    prepared = null;
    markValidation(
      CONNECTION_ROLES,
      "stale",
      "Server changed; checking both paths again.",
    );
    return;
  }
  if (
    event.type === "error" &&
    CONNECTION_FAILURE_REASONS.has(event.error.reason)
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
  // A rejection is already recorded as a "failed" role by markValidation.
  if (booted && !store.isRunning)
    void validateConnections(true).catch(() => {});
}

// A hidden tab does no background work: the keepalive's ping socket and worker
// stop so the browser can park the page. A run keeps going, since hiding the
// tab mid-measurement must not disturb it. Returning re-arms the keepalive and
// re-checks a stale cached probe.
function refreshAfterVisibility() {
  const hidden = document.visibilityState === "hidden";
  // Safe during a run: the keepalive is already stopped for its duration, and
  // the flag decides whether it comes back when the run ends.
  runner?.setBackgroundActivity?.(!hidden);
  if (hidden) {
    hiddenAt = Date.now();
  } else if (hiddenAt && Date.now() - hiddenAt >= CONNECTION_FRESH_MS) {
    hiddenAt = 0;
    refreshAfterTransition();
  }
}

export async function bootRunner() {
  const engine = getRunner();
  store.engineInfo = engine.describe();
  unsubscribe = engine.on(ingestRunnerEvent);
  booted = true;
  for (const role of CONNECTION_ROLES)
    lastDraftRoleKeys[role] = connectionDraftRoleKey(store.config, role);
  window.addEventListener("online", refreshAfterTransition);
  document.addEventListener("visibilitychange", refreshAfterVisibility);
  // A tab opened in the background gets no visibilitychange, so seed the flag
  // from the current state instead of assuming the page is visible.
  refreshAfterVisibility();
  // A rejection is already recorded as a "failed" role by markValidation.
  await validateConnections().catch(() => {});
}

export function toggleRun() {
  if (store.isRunning) {
    cancelPendingStart();
    getRunner().abort();
    return;
  }
  if (pendingStartAbort) return;
  const cfg = $state.snapshot(store.config);
  const key = connectionKey(cfg, store.transportDiscovery);
  const startAbort = new AbortController();
  const startSeq = ++pendingStartSeq;
  pendingStartAbort = startAbort;
  store.startPending = true;
  const current = () =>
    pendingStartSeq === startSeq && !startAbort.signal.aborted;
  const start = async () => {
    const budget = await requireSessionCoverage(
      buildSegments(cfg).totalMs + SESSION_RUN_MARGIN_MS,
      startAbort.signal,
    );
    if (!current()) return;
    sessionBudget = budget;
    store.reset();
    store.startPending = true;
    if (!current()) return;
    const info = preparedIsFresh(key)
      ? prepared!.info
      : await validateConnections();
    if (!current()) return;
    if (!preparedIsFresh(connectionKey(cfg, store.transportDiscovery))) return;
    store.activeConfig = structuredClone(cfg);
    store.activeConnections = $state.snapshot(store.connections);
    if (!current()) return;
    await getRunner().start(cfg, info);
  };
  start()
    .catch((cause) => {
      if (!current()) return;
      if (cause instanceof SessionCoverageError) {
        store.startError = cause.message;
        return;
      }
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
    })
    .finally(() => {
      if (pendingStartAbort === startAbort) {
        pendingStartAbort = null;
        store.startPending = false;
      }
    });
}

export function hasPendingStart(): boolean {
  return pendingStartAbort !== null;
}

/**
 * Return to the fresh, blank idle view, matching a page reload's starting state
 * (persisted settings are untouched). Aborts any in-flight run first, which is
 * synchronous, so the reset that follows sticks; then clears samples, result,
 * and phase back to idle.
 */
export function returnToStart() {
  cancelPendingStart();
  if (store.isRunning) getRunner().abort();
  store.reset();
  // A rejection is already recorded as a "failed" role by markValidation.
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
  const activeTotal = store.activeConfig
    ? buildSegments(store.activeConfig).totalMs
    : 0;
  const candidateTotal = buildSegments(config).totalMs;
  if (
    !liveScheduleFitsSession(
      sessionBudget,
      activeTotal,
      candidateTotal,
      SESSION_RUN_MARGIN_MS,
    )
  ) {
    if (store.activeConfig) {
      store.config.stages = structuredClone(store.activeConfig.stages);
      store.config.duration = structuredClone(store.activeConfig.duration);
    }
    store.startError =
      "This change would extend the test beyond the current session.";
    return;
  }
  store.startError = "";
  store.compactThroughputForDuration(candidateTotal);
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
  cancelPendingStart();
  validationSeq++;
  runner?.dispose?.();
  runner = null;
  prepared = null;
  pendingValidation = false;
  validating = [];
  validationAbort?.abort();
  validationAbort = null;
  window.removeEventListener("online", refreshAfterTransition);
  document.removeEventListener("visibilitychange", refreshAfterVisibility);
  unsubscribe?.();
  unsubscribe = null;
  store.reset();
  // Server-scoped evidence, cleared with the server binding rather than in
  // `store.reset()`: a run resets the store but keeps talking to the same
  // server, and several surfaces read `store.infra` without the connection
  // presentation's generation gate.
  store.transportDiscovery = null;
  store.infra = null;
}
