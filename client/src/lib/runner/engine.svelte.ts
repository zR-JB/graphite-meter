import type {
  ConnectionRole,
  InfraInfo,
  LiveRunConfig,
  NetworkRunner,
  RunnerEvent,
} from "./contract";
import { RunnerCore } from "./core";
import { DummyBackend } from "./dummy";
import {
  PreflightUnavailableError,
  RealBackend,
  TransportUnavailableError,
} from "./RealRunner";
import { store } from "../state/store.svelte";
import { canonicalAdaptiveConfig } from "../state/defaults";
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
  connectionFailureBackoff,
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
let validating: ConnectionRole[] = [];
let sessionBudget: SessionBudget | null = null;
let validationTimer: ReturnType<typeof setTimeout> | null = null;

let validationDueAt = 0;
let lastValidationAttemptAt = 0;
let validationFailureCount = 0;
let connectivityOnline: boolean | null = null;
const SESSION_RUN_MARGIN_MS = 60_000;

function preparationPathState(
  role: ConnectionRole,
): "checking" | "ready" | "failed" | "stale" | "disabled" {
  if (
    role === "throughput" &&
    !(
      store.config.stages.download ||
      store.config.stages.upload ||
      store.config.stages.bidirectional
    )
  )
    return "disabled";
  if (role === "latency" && !store.latencyEnabled) return "disabled";
  const state = store.connectionValidation[role].state;
  if (state === "verified") return "ready";
  return state;
}

function updatePreparation(
  status: "authenticating" | "checking" | "launching" | "failed" | "idle",
): void {
  store.preparation = {
    status,
    throughput: preparationPathState("throughput"),
    latency: preparationPathState("latency"),
  };
}

function clearValidationTimer(): void {
  if (validationTimer !== null) clearTimeout(validationTimer);
  validationTimer = null;
}

function validationHidden(): boolean {
  return document.visibilityState === "hidden";
}

function validationDue(): number {
  const now = Date.now();
  if (validationDueAt) return validationDueAt;
  if (prepared) return prepared.verifiedAt + CONNECTION_FRESH_MS;
  if (lastValidationAttemptAt)
    return lastValidationAttemptAt + CONNECTION_FRESH_MS;
  return now;
}

function scheduleValidation(): void {
  clearValidationTimer();
  if (!booted || store.isRunning || validationHidden()) return;
  const dueAt = validationDue();
  const delay = Math.max(0, dueAt - Date.now());
  validationTimer = setTimeout(() => {
    validationTimer = null;
    serviceValidation();
  }, delay);
}

function requestValidation(): void {
  validationDueAt = Date.now();
  scheduleValidation();
}

function serviceValidation(): void {
  if (!booted || store.isRunning || validationHidden()) {
    scheduleValidation();
    return;
  }
  if (validating.length) return;
  if (validationDue() > Date.now()) {
    scheduleValidation();
    return;
  }
  validationDueAt = 0;
  queueMicrotask(
    () =>
      void validateConnections(true).catch(() => {
        // Validation state carries expected probe failures to the UI.
      }),
  );
}

export function cancelPendingStart() {
  if (!pendingStartAbort) {
    if (store.preparation.status !== "idle") updatePreparation("idle");
    return;
  }
  pendingStartSeq++;
  pendingStartAbort.abort();
  pendingStartAbort = null;
  store.startError = "";
  updatePreparation("idle");
}

if (typeof window !== "undefined") {
  $effect.root(() => {
    $effect(() => {
      const changed = CONNECTION_ROLES.filter(
        (role) =>
          connectionDraftRoleKey(store.config, role) !==
          lastDraftRoleKeys[role],
      );
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
        if (changed.includes("latency") && needed.includes("latency"))
          store.idleLatency = [];
        for (const role of changed)
          lastDraftRoleKeys[role] = connectionDraftRoleKey(store.config, role);
        if (!needed.length) return;
        requestValidation();
        if (running) {
          markValidation(
            changed.filter((role) => needed.includes(role)),
            "stale",
            "Draft changed; validation resumes after this run.",
          );
          return;
        }
      }
      serviceValidation();
    });
  });
}

export function getRunner(): NetworkRunner {
  if (!runner) {
    const dummy =
      __GM_ALLOW_DUMMY__ &&
      typeof window !== "undefined" &&
      new URLSearchParams(window.location.search).get("engine") === "dummy";
    runner = new RunnerCore(dummy ? new DummyBackend() : new RealBackend());
  }
  return runner;
}

function isNetworkUnavailable(cause: unknown): boolean {
  const seen = new Set<object>();
  let current: unknown = cause;
  while (
    current &&
    (typeof current === "object" || typeof current === "function")
  ) {
    const value = current as {
      name?: unknown;
      message?: unknown;
      cause?: unknown;
    };
    if (seen.has(value)) return false;
    seen.add(value);
    const name = typeof value.name === "string" ? value.name : "";
    const message = typeof value.message === "string" ? value.message : "";
    if (
      name === "NetworkError" ||
      /failed to fetch|fetch failed|network(?:error| request failed)|load failed|connection (?:refused|reset|lost)/i.test(
        message,
      )
    )
      return true;
    current = value.cause;
  }
  return false;
}

export function connectionFailureMessage(cause: unknown): string {
  return cause instanceof PreflightUnavailableError &&
    isNetworkUnavailable(cause.cause)
    ? "Server could not be reached"
    : "Connection check failed";
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
  if (store.preparing)
    updatePreparation(
      store.preparation.status === "launching" ? "launching" : "checking",
    );
}

function preparedIsFresh(key: string): boolean {
  const config = $state.snapshot(store.config);
  if (
    CONNECTION_ROLES.some((role) =>
      roleNeedsValidation(
        config,
        store.connectionValidation,
        role,
        store.transportDiscovery,
      ),
    )
  )
    return false;
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
  ownerSignal?: AbortSignal,
): Promise<InfraInfo> {
  if (!booted) throw new DOMException("Runner is not active", "AbortError");
  const config = $state.snapshot(store.config);
  const key = connectionKey(config, store.transportDiscovery);
  const draftKey = connectionDraftKey(config);
  if (!force && preparedIsFresh(key)) return prepared!.info;
  lastValidationAttemptAt = Date.now();
  validationDueAt = 0;
  if (validating.length) markValidation(validating, "stale");
  validationAbort?.abort();
  const roles = validationRoles(
    config,
    store.connectionValidation,
    requestedRole,
    store.transportDiscovery,
  );
  const abort = new AbortController();
  const relayOwnerAbort = () => abort.abort();
  ownerSignal?.addEventListener("abort", relayOwnerAbort, { once: true });
  if (ownerSignal?.aborted) abort.abort();
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
          cause instanceof PreflightUnavailableError
            ? CONNECTION_ROLES
            : cause instanceof TransportUnavailableError && cause.role
              ? [cause.role]
              : batchRoles;
        markValidation(
          failedRoles,
          "failed",
          connectionFailureMessage(cause),
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
      if (firstFailure instanceof PreflightUnavailableError)
        store.connectivity = "offline";
      validationFailureCount++;
      validationDueAt =
        Date.now() + connectionFailureBackoff(validationFailureCount);
      throw firstFailure;
    }
    if (!latest) throw new Error("no connection role was validated");
    prepared = {
      key: connectionKey(config, store.transportDiscovery),
      info: latest,
      verifiedAt: Date.now(),
    };
    validationFailureCount = 0;
    validationDueAt = prepared.verifiedAt + CONNECTION_FRESH_MS;
    return latest;
  } finally {
    if (validationAbort === abort) {
      validationAbort = null;
      validating = [];
    }
    ownerSignal?.removeEventListener("abort", relayOwnerAbort);
    scheduleValidation();
  }
}

function ingestRunnerEvent(event: RunnerEvent) {
  if (event.type === "connectivity") {
    if (event.state === "offline") refreshAfterOffline();
    else refreshAfterTransition();
  }
  if (
    event.type === "phase" &&
    event.transition.to === "connecting" &&
    pendingStartAbort
  ) {
    pendingStartAbort = null;
  }
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
  if (
    event.type === "phase" &&
    ["complete", "aborted", "error"].includes(event.transition.to)
  )
    scheduleValidation();
}

function refreshAfterTransition() {
  const active = validating.length > 0;
  if (
    connectivityOnline === true &&
    !CONNECTION_ROLES.some((role) =>
      roleNeedsValidation(
        store.config,
        store.connectionValidation,
        role,
        store.transportDiscovery,
      ),
    )
  )
    return;
  connectivityOnline = true;
  if (!active) requestValidation();
}

function refreshAfterOffline() {
  if (connectivityOnline === false) return;
  connectivityOnline = false;
  prepared = null;
  markValidation(CONNECTION_ROLES, "stale", "Connection changed; check again.");
  if (!validating.length) requestValidation();
}

function refreshAfterVisibility() {
  const hidden = document.visibilityState === "hidden";
  runner?.setBackgroundActivity?.(!hidden);
  if (hidden) {
    clearValidationTimer();
  } else {
    scheduleValidation();
  }
}

export async function bootRunner() {
  const engine = getRunner();
  store.engineInfo = engine.describe();
  unsubscribe = engine.on(ingestRunnerEvent);
  booted = true;
  connectivityOnline =
    typeof navigator === "undefined" ? null : navigator.onLine;
  for (const role of CONNECTION_ROLES)
    lastDraftRoleKeys[role] = connectionDraftRoleKey(store.config, role);
  window.addEventListener("online", refreshAfterTransition);
  window.addEventListener("offline", refreshAfterOffline);
  document.addEventListener("visibilitychange", refreshAfterVisibility);
  refreshAfterVisibility();
  if (validationHidden()) requestValidation();
  else await validateConnections().catch(() => {});
}

export function toggleRun() {
  if (store.isRunning) {
    cancelPendingStart();
    getRunner().abort();
    return;
  }
  if (pendingStartAbort) {
    cancelPendingStart();
    return;
  }
  const cfg = $state.snapshot(store.config);
  cfg.adaptive = canonicalAdaptiveConfig(cfg.adaptive);
  const key = connectionKey(cfg, store.transportDiscovery);
  const startAbort = new AbortController();
  const startSeq = ++pendingStartSeq;
  pendingStartAbort = startAbort;
  store.startError = "";
  updatePreparation("authenticating");
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
    updatePreparation("checking");
    if (!current()) return;
    const info = preparedIsFresh(key)
      ? prepared!.info
      : await validateConnections(false, undefined, startAbort.signal);
    if (!current()) return;
    if (!preparedIsFresh(connectionKey(cfg, store.transportDiscovery))) return;
    updatePreparation("launching");
    store.activeConfig = structuredClone(cfg);
    store.activeConnections = $state.snapshot(store.connections);
    if (!current()) return;
    await getRunner().start(cfg, info);
  };
  start()
    .catch((cause) => {
      if (!current()) return;
      if (cause instanceof DOMException && cause.name === "AbortError") return;
      if (cause instanceof SessionCoverageError) {
        store.startError = cause.message;
        updatePreparation("failed");
        return;
      }
      store.startError = connectionFailureMessage(cause);
      updatePreparation("failed");
    })
    .finally(() => {
      if (pendingStartAbort === startAbort) {
        pendingStartAbort = null;
        if (store.preparation.status === "launching") updatePreparation("idle");
      }
    });
}

export function hasPendingStart(): boolean {
  return pendingStartAbort !== null;
}

export function returnToStart() {
  cancelPendingStart();
  if (store.isRunning) getRunner().abort();
  store.reset();
  requestValidation();
}

export function applyLiveRunConfig() {
  if (!store.isRunning) return;
  const config = $state.snapshot(store.config);
  config.adaptive = canonicalAdaptiveConfig(config.adaptive);
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

export function teardownRunner() {
  booted = false;
  cancelPendingStart();
  validationSeq++;
  runner?.dispose?.();
  runner = null;
  prepared = null;
  validating = [];
  validationAbort?.abort();
  validationAbort = null;
  clearValidationTimer();
  validationDueAt = 0;
  lastValidationAttemptAt = 0;
  validationFailureCount = 0;
  connectivityOnline = null;
  window.removeEventListener("online", refreshAfterTransition);
  window.removeEventListener("offline", refreshAfterOffline);
  document.removeEventListener("visibilitychange", refreshAfterVisibility);
  unsubscribe?.();
  unsubscribe = null;
  store.reset();
  store.transportDiscovery = null;
  store.infra = null;
}
