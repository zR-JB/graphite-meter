import type {
  ConnectionRole,
  EngineInfo,
  LiveRunConfig,
  NetworkRunner,
  PreparedPaths,
  RunnerEvent,
} from "./contract";
import { RunnerCore } from "./core";
import { DummyBackend } from "./dummy";
import { RealBackend } from "./RealRunner";
import { PreflightUnavailableError } from "./real/transportError";
import { prepareConnections, type ConnectionPreparation } from "./real/prepare";
import type {
  store as applicationStore,
  StageKey,
} from "../state/store.svelte";
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
  validationRoles,
  preparedPaths,
  emptyConnectionValidation,
  latencyPathNeeded,
} from "./connectionModel";

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

interface ApplicationDependencies {
  prepare: typeof prepareConnections;
  createRunner: (paths: PreparedPaths) => NetworkRunner;
  describe: () => EngineInfo;
}

export function createApplicationController(
  store: typeof applicationStore,
  dependencies: Partial<ApplicationDependencies> = {},
) {
  const dummy =
    __GM_ALLOW_DUMMY__ &&
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).get("engine") === "dummy";
  const prepare =
    dependencies.prepare ?? (dummy ? DummyBackend.prepare : prepareConnections);
  const createRunner =
    dependencies.createRunner ??
    ((paths: PreparedPaths) =>
      new RunnerCore(dummy ? new DummyBackend() : new RealBackend(paths)));
  const describe =
    dependencies.describe ??
    (dummy ? DummyBackend.describe : RealBackend.describe);
  let runner: NetworkRunner | null = null;
  let unsubscribe: (() => void) | undefined;
  let disposeDraft: (() => void) | undefined;
  let idle: NonNullable<ConnectionPreparation["idle"]> | null = null;
  let validation: {
    abort: AbortController;
    roles: ConnectionRole[];
    draft: string;
  } | null = null;
  let pendingStart: { abort: AbortController; draft: string } | null = null;
  let booted = false;
  let sessionBudget: SessionBudget | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let nextValidationAt = 0;
  let failures = 0;
  let online: boolean | null = null;
  const SESSION_RUN_MARGIN_MS = 60_000;

  const freshPaths = () =>
    preparedPaths(
      store.config,
      store.transportDiscovery,
      store.connectionValidation,
    );
  const hidden = () => document.visibilityState === "hidden";
  function mark(
    roles: ConnectionRole[],
    state: ConnectionValidationState,
    message?: string,
  ) {
    const next = { ...store.connectionValidation };
    if (roles.includes("throughput"))
      next.throughput = { ...next.throughput, state, message };
    if (roles.includes("latency"))
      next.latency = { ...next.latency, state, message };
    store.connectionValidation = next;
  }
  function schedule() {
    clearTimeout(timer);
    timer = undefined;
    if (!booted || store.isRunning || pendingStart || validation || hidden())
      return;
    timer = setTimeout(
      () => {
        timer = undefined;
        void validateConnections(true).catch(() => {});
      },
      Math.max(0, nextValidationAt - Date.now()),
    );
  }
  function requestValidation() {
    nextValidationAt = Date.now();
    schedule();
  }
  function cancelPendingStart() {
    pendingStart?.abort.abort();
    pendingStart = null;
    store.startError = "";
    store.preparationStatus = "idle";
    schedule();
  }
  function refreshIdle() {
    if (
      !booted ||
      store.isRunning ||
      hidden() ||
      !latencyPathNeeded(store.config)
    )
      idle?.stop();
    else idle?.start();
  }
  function ingest(event: RunnerEvent) {
    if (event.type === "connectivity") {
      if (event.state === "offline") offline();
      else onlineAgain();
    }
    if (
      event.type === "error" &&
      CONNECTION_FAILURE_REASONS.has(event.error.reason)
    )
      mark(CONNECTION_ROLES, "stale", "Connection changed; check again.");
    store.ingest(event);
    if (
      event.type === "complete" ||
      event.type === "error" ||
      event.type === "phase"
    ) {
      refreshIdle();
      schedule();
    }
  }
  function offline() {
    if (online === false) return;
    online = false;
    mark(CONNECTION_ROLES, "stale", "Connection changed; check again.");
    requestValidation();
  }
  function onlineAgain() {
    if (
      online === true &&
      CONNECTION_ROLES.every(
        (role) =>
          !roleNeedsValidation(
            store.config,
            store.connectionValidation,
            role,
            store.transportDiscovery,
          ),
      )
    )
      return;
    online = true;
    requestValidation();
  }
  function visibilityChanged() {
    refreshIdle();
    schedule();
  }

  async function validateConnections(
    force = false,
    requestedRole?: ConnectionRole,
    ownerSignal?: AbortSignal,
  ): Promise<void> {
    if (!booted) throw new DOMException("Runner is not active", "AbortError");
    if (!force && freshPaths()) return;
    const config = $state.snapshot(store.config);
    const draft = connectionDraftKey(config);
    if (validation) {
      validation.abort.abort();
      mark(validation.roles, "stale");
    }
    const task = {
      abort: new AbortController(),
      draft,
      roles: validationRoles(
        config,
        store.connectionValidation,
        requestedRole,
        store.transportDiscovery,
      ),
    };
    const previous = store.connectionValidation;
    validation = task;
    const signal = ownerSignal
      ? AbortSignal.any([ownerSignal, task.abort.signal])
      : task.abort.signal;
    const current = () =>
      booted &&
      validation === task &&
      !signal.aborted &&
      connectionDraftKey(store.config) === draft;
    mark(task.roles, "checking");
    nextValidationAt = Date.now() + CONNECTION_FRESH_MS;
    let result: ConnectionPreparation | undefined;
    try {
      result = await prepare(config, previous, task.roles, signal);
      if (!current()) throw new DOMException("Aborted", "AbortError");
      store.transportDiscovery = result.discovery;
      store.connectionValidation = result.validation;
      if (result.idle !== undefined) {
        idle?.stop();
        idle = result.idle;
        if (idle) idle.onEvent = ingest;
        refreshIdle();
      }
      if (result.failure) throw result.failure;
      if (!freshPaths()) {
        // A provisional monitor may have stalled before adoption; its observed edge invalidates these checks.
        nextValidationAt = Date.now() + connectionFailureBackoff(++failures);
      } else {
        failures = 0;
        const checked = CONNECTION_ROLES.flatMap(
          (role) => result!.validation[role].path?.verifiedAt ?? [],
        );
        nextValidationAt = Math.min(...checked) + CONNECTION_FRESH_MS;
      }
    } catch (cause) {
      if (!current()) {
        if (result?.idle !== idle) result?.idle?.stop();
        throw cause;
      }
      if (!result) {
        mark(CONNECTION_ROLES, "failed", connectionFailureMessage(cause));
        if (cause instanceof PreflightUnavailableError)
          store.connectivity = "offline";
      }
      nextValidationAt = Date.now() + connectionFailureBackoff(++failures);
      throw cause;
    } finally {
      if (validation === task) {
        if (signal.aborted) mark(task.roles, "stale");
        validation = null;
      }
      schedule();
    }
  }

  async function boot() {
    if (booted) return;
    booted = true;
    store.engineInfo = describe();
    online = typeof navigator === "undefined" ? null : navigator.onLine;
    let draftKeys = CONNECTION_ROLES.map((role) =>
      connectionDraftRoleKey(store.config, role),
    );
    disposeDraft = $effect.root(() => {
      $effect(() => {
        const keys = CONNECTION_ROLES.map((role) =>
          connectionDraftRoleKey(store.config, role),
        );
        const changed = CONNECTION_ROLES.filter(
          (_role, i) => keys[i] !== draftKeys[i],
        );
        const needed = changed.filter((role) =>
          roleNeedsValidation(
            store.config,
            store.connectionValidation,
            role,
            store.transportDiscovery,
          ),
        );
        draftKeys = keys;
        if (!booted || !changed.length) return;
        const draft = connectionDraftKey(store.config);
        if (pendingStart && pendingStart.draft !== draft) cancelPendingStart();
        if (validation && validation.draft !== draft) validation.abort.abort();
        if (changed.includes("latency") && !latencyPathNeeded(store.config)) {
          idle?.stop();
          idle = null;
          store.connectionValidation = {
            ...store.connectionValidation,
            latency: {
              selection: store.config.transports.latencyTarget,
              state: "stale",
              path: null,
            },
          };
        }
        if (!needed.length || validation?.draft === draft) return;
        if (needed.includes("latency")) store.idleLatency = [];
        mark(
          needed,
          "stale",
          store.isRunning
            ? "Draft changed; validation resumes after this run."
            : undefined,
        );
        requestValidation();
      });
    });
    window.addEventListener("online", onlineAgain);
    window.addEventListener("offline", offline);
    document.addEventListener("visibilitychange", visibilityChanged);
    if (hidden()) requestValidation();
    else await validateConnections().catch(() => {});
  }

  function toggleRun() {
    if (!booted) return;
    if (store.isRunning) {
      cancelPendingStart();
      runner?.abort();
      return;
    }
    if (pendingStart) {
      cancelPendingStart();
      return;
    }
    const config = $state.snapshot(store.config);
    config.adaptive = canonicalAdaptiveConfig(config.adaptive);
    const abort = new AbortController();
    pendingStart = { abort, draft: connectionDraftKey(config) };
    store.startError = "";
    store.preparationStatus = "authenticating";
    const current = () =>
      booted &&
      pendingStart?.abort === abort &&
      !abort.signal.aborted &&
      connectionDraftKey(store.config) === connectionDraftKey(config);
    const start = async () => {
      const budget = await requireSessionCoverage(
        buildSegments(config).totalMs + SESSION_RUN_MARGIN_MS,
        abort.signal,
      );
      if (!current()) return;
      sessionBudget = budget;
      store.reset();
      store.preparationStatus = "checking";
      await validateConnections(false, undefined, abort.signal);
      if (!current()) return;
      const paths = freshPaths();
      if (!paths) throw new Error("connection evidence expired before start");
      store.preparationStatus = "launching";
      unsubscribe?.();
      runner?.dispose();
      runner = createRunner(paths);
      unsubscribe = runner.on(ingest);
      store.activeConfig = structuredClone(config);
      store.activePaths = paths;
      idle?.stop();
      runner.start(config, paths.latency?.rttMs ?? 0);
    };
    void start()
      .catch((cause) => {
        if (!current()) return;
        if (cause instanceof DOMException && cause.name === "AbortError")
          return;
        store.startError =
          cause instanceof SessionCoverageError
            ? cause.message
            : connectionFailureMessage(cause);
        store.preparationStatus = "failed";
      })
      .finally(() => {
        if (pendingStart?.abort === abort) {
          pendingStart = null;
          if (store.preparationStatus === "launching")
            store.preparationStatus = "idle";
        }
        schedule();
      });
  }
  function returnToStart() {
    cancelPendingStart();
    runner?.abort();
    store.reset();
    requestValidation();
  }
  function configureRun(patch: Partial<LiveRunConfig>): boolean {
    const config = { ...$state.snapshot(store.config), ...patch };
    config.adaptive = canonicalAdaptiveConfig(config.adaptive);
    if (!Object.values(config.stages).some(Boolean)) return false;
    if (
      Object.values(config.duration).some(
        (value) => !Number.isFinite(value) || value < 0,
      )
    )
      return false;
    if (
      (Object.keys(config.stages) as StageKey[]).some(
        (stage) =>
          config.stages[stage] !== store.config.stages[stage] &&
          !store.canToggleStage(stage),
      )
    )
      return false;
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
      store.isRunning &&
      !liveScheduleFitsSession(
        sessionBudget,
        activeTotal,
        candidateTotal,
        SESSION_RUN_MARGIN_MS,
      )
    ) {
      store.startError =
        "This change would extend the test beyond the current session.";
      return false;
    }
    cancelPendingStart();
    store.config = config;
    store.startError = "";
    if (!store.isRunning) return true;
    store.compactThroughputForDuration(candidateTotal);
    runner?.reconfigure(live);
    if (store.activeConfig)
      store.activeConfig = { ...store.activeConfig, ...live };
    return true;
  }

  function dispose() {
    booted = false;
    disposeDraft?.();
    cancelPendingStart();
    validation?.abort.abort();
    validation = null;
    unsubscribe?.();
    runner?.dispose();
    runner = null;
    idle?.stop();
    idle = null;
    clearTimeout(timer);
    window.removeEventListener("online", onlineAgain);
    window.removeEventListener("offline", offline);
    document.removeEventListener("visibilitychange", visibilityChanged);
    store.reset();
    store.transportDiscovery = null;
    store.connectionValidation = emptyConnectionValidation();
  }
  function toggleStage(stage: StageKey): boolean {
    if (!store.canToggleStage(stage)) return false;
    const stages = { ...$state.snapshot(store.config.stages) };
    if (stages[stage] && Object.values(stages).filter(Boolean).length <= 1)
      return false;
    stages[stage] = !stages[stage];
    return configureRun({ stages });
  }
  function selectConnection(role: ConnectionRole, value: string) {
    cancelPendingStart();
    if (role === "throughput") store.config.transports.throughputTarget = value;
    else store.config.transports.latencyTarget = value;
  }
  return {
    boot,
    dispose,
    toggleRun,
    cancelPendingStart,
    hasPendingStart: () => pendingStart !== null,
    returnToStart,
    validateConnections,
    configureRun,
    toggleStage,
    selectConnection,
  };
}
export type ApplicationController = ReturnType<
  typeof createApplicationController
>;
