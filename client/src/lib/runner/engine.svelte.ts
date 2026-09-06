import { readJSONResponse } from "../api/decode";
import { authenticatedFetch } from "../auth";
import {
  parseCatalog,
  reconcileSelection,
  selectedInCatalogOrder,
  validateSelection,
  type ServerEntry,
} from "../servers/catalog";
import {
  browserApproval,
  serverCredentials,
  ServerAuthenticationRequired,
  type ServerCredentials,
} from "../servers/credentials";
import { ServerCoordinator } from "../servers/coordinator";
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
  AUTHENTICATION_REQUIRED_EVENT,
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
  let lifetime = new AbortController();
  let catalogCheck: AbortController | null = null;
  let sessionBudget: SessionBudget | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let nextValidationAt = 0;
  let failures = 0;
  let online: boolean | null = null;
  const SESSION_RUN_MARGIN_MS = 60_000;

  const useCatalog = !dummy && !dependencies.prepare;
  const contexts = new Map<string, ServerCredentials>();
  const serverChecks = new Map<string, AbortController>();
  const selectedPaths = new Map<string, PreparedPaths>();
  const selectedValidation = new Map<
    string,
    ConnectionPreparation["validation"]
  >();
  const selectedIdle = new Map<
    string,
    NonNullable<ConnectionPreparation["idle"]>
  >();
  let inspection: AbortController | null = null;
  let approval: AbortController | null = null;
  const selectionKey = () =>
    JSON.stringify([
      store.latencySelection.mode,
      store.primaryLatencyServer,
      store.selectedServers.map((id) => [
        id,
        store.serverCatalog?.servers.find((server) => server.id === id)?.url,
      ]),
    ]);
  const serverConfig = (id: string) => {
    const config = $state.snapshot(store.config);
    if (
      store.selectedServers.includes(id) &&
      store.latencySelection.mode === "primary" &&
      id !== store.primaryLatencyServer
    ) {
      config.stages.latency = false;
      config.skipLoadedLatencyWhenStageOff = true;
    }
    return config;
  };
  const draftKey = (config: typeof store.config) =>
    useCatalog
      ? JSON.stringify([connectionDraftKey(config), selectionKey()])
      : connectionDraftKey(config);
  const catalogSelected = () =>
    selectedInCatalogOrder(store.serverCatalog!, store.selectedServers);
  const readySelected = () =>
    useCatalog &&
    store.serverCatalog &&
    !store.unresolvedServers.length &&
    store.selectedServers.length > 0 &&
    store.selectedServers.every(
      (id) =>
        store.serverReadiness[id]?.state === "ready" &&
        selectedPaths.has(id) &&
        !!preparedPaths(
          serverConfig(id),
          store.serverDiscoveries[id] ?? null,
          selectedValidation.get(id) ?? emptyConnectionValidation(),
        ),
    );
  const savedSelectionKey = "graphite-meter:server-selection:v1";

  async function loadCatalog() {
    catalogCheck?.abort();
    const check = new AbortController();
    catalogCheck = check;
    const signal = AbortSignal.any([
      check.signal,
      lifetime.signal,
      AbortSignal.timeout(5000),
    ]);
    store.catalogLoading = true;
    try {
      const response = await authenticatedFetch("/servers", {
        cache: "no-store",
        signal,
      });
      if (!response.ok) throw new Error("Could not load the server catalogue");
      const catalog = parseCatalog(
        await readJSONResponse(response),
        location.origin,
      );
      signal.throwIfAborted();
      validation?.abort.abort();
      inspection?.abort();
      for (const check of serverChecks.values()) check.abort();
      serverChecks.clear();
      const previousContexts = new Map(contexts);
      contexts.clear();
      selectedPaths.clear();
      selectedValidation.clear();
      for (const monitor of selectedIdle.values()) monitor.stop();
      selectedIdle.clear();
      store.serverDiscoveries = {};
      store.serverReadiness = {};
      store.serverCatalog = catalog;
      let saved: unknown;
      try {
        saved = JSON.parse(localStorage.getItem(savedSelectionKey) ?? "null");
      } catch {
        saved = null;
      }
      const selection = reconcileSelection(catalog, saved);
      store.selectedServers = selection.ids;
      store.unresolvedServers = selection.unresolved;
      for (const server of catalog.servers) {
        const previous = previousContexts.get(server.id);
        contexts.set(
          server.id,
          previous?.server.url === server.url
            ? { ...previous, server }
            : serverCredentials(server),
        );
        store.serverReadiness[server.id] = { state: "unchecked" };
      }
      if (selection.unresolved.length)
        store.startError =
          "Saved servers have changed. Review the selection before starting.";
    } finally {
      if (catalogCheck === check) {
        catalogCheck = null;
        store.catalogLoading = false;
      }
    }
  }
  async function retryCatalogue() {
    if (!booted || store.isRunning || store.preparing) return;
    try {
      await loadCatalog();
      await validateServers(true);
    } catch (cause) {
      if (!lifetime.signal.aborted)
        store.startError =
          cause instanceof Error ? cause.message : "Could not load servers";
    }
  }
  async function inspectServer(
    server: ServerEntry,
    ownerSignal: AbortSignal,
    roles: ConnectionRole[] = CONNECTION_ROLES,
  ): Promise<void> {
    serverChecks.get(server.id)?.abort();
    const check = new AbortController();
    serverChecks.set(server.id, check);
    const signal = AbortSignal.any([
      ownerSignal,
      check.signal,
      lifetime.signal,
    ]);
    const configKey = draftKey(store.config);
    const current = () =>
      serverChecks.get(server.id) === check &&
      configKey === draftKey(store.config);
    store.serverReadiness[server.id] = { state: "checking" };
    const config = serverConfig(server.id);
    let result: ConnectionPreparation | undefined;
    try {
      result = await prepare(
        config,
        selectedValidation.get(server.id) ?? emptyConnectionValidation(),
        roles,
        signal,
        contexts.get(server.id),
      );
      signal.throwIfAborted();
      if (!current()) throw new DOMException("Aborted", "AbortError");
      store.serverDiscoveries[server.id] = result.discovery;
      const entry = store.serverCatalog?.servers.find(
        (entry) => entry.id === server.id,
      );
      if (entry) {
        entry.name = result.discovery.server.name || entry.name;
        entry.location = result.discovery.server.location;
        const context = contexts.get(server.id);
        if (context)
          context.server = {
            ...context.server,
            name: entry.name,
            location: entry.location,
          };
      }
      selectedValidation.set(server.id, result.validation);
      if (result.failure) throw result.failure;
      if (
        (config.stages.upload || config.stages.bidirectional) &&
        !result.discovery.uploadCheckpoint
      )
        throw new Error(
          `${server.name} needs receiver checkpoint support for coordinated uploads. Upgrade this measurement server.`,
        );
      const paths = preparedPaths(config, result.discovery, result.validation);
      if (!paths) throw new Error("Enabled measurements could not be prepared");
      paths.credentials = contexts.get(server.id);
      selectedPaths.set(server.id, paths);
      selectedIdle.get(server.id)?.stop();
      if (result.idle) {
        result.idle.stop();
        selectedIdle.set(server.id, result.idle);
      }
      store.serverReadiness[server.id] = {
        state: "ready",
        message: `Throughput: ${paths.throughput.target.transport} · ${paths.throughput.target.origin}${paths.latency ? `; latency: ${paths.latency.target.transport} · ${paths.latency.target.origin}` : ""}`,
        checkedAt: Date.now(),
      };
    } catch (cause) {
      result?.idle?.stop();
      if (!current()) throw cause;
      selectedPaths.delete(server.id);
      if (signal.aborted) {
        store.serverReadiness[server.id] = { state: "unchecked" };
        throw cause;
      }
      if (!result) {
        const previous =
          selectedValidation.get(server.id) ?? emptyConnectionValidation();
        const message = connectionFailureMessage(cause);
        selectedValidation.set(server.id, {
          throughput: { ...previous.throughput, state: "failed", message },
          latency: { ...previous.latency, state: "failed", message },
        });
      }
      let error: unknown = cause;
      while (
        error instanceof Error &&
        !(error instanceof ServerAuthenticationRequired) &&
        error.cause
      )
        error = error.cause;
      store.serverReadiness[server.id] = {
        state:
          error instanceof ServerAuthenticationRequired ? "sign-in" : "failed",
        message:
          error instanceof Error
            ? error.message
            : "Server could not be reached",
        checkedAt: Date.now(),
      };
      throw cause;
    }
  }
  function adoptSelectedEvidence() {
    // Keep the representative paths and discovery in one server generation.
    const selected = catalogSelected();
    const first =
      store.latencySelection.mode === "primary"
        ? selected.find((server) => server.id === store.primaryLatencyServer)!
        : selected[0];
    const paths = selectedPaths.get(first.id);
    const checked = selectedValidation.get(first.id);
    const discovery = store.serverDiscoveries[first.id];
    if (discovery) store.transportDiscovery = discovery;
    if (checked) store.connectionValidation = checked;
    else
      mark(
        CONNECTION_ROLES,
        "failed",
        "Resolve the selected servers before starting.",
      );
    for (const monitor of selectedIdle.values()) monitor.stop();
    if (paths?.latency) {
      idle = selectedIdle.get(first.id) ?? null;
      if (idle) {
        idle.onEvent = ingest;
        refreshIdle();
      }
    }
  }
  async function validateServers(
    force: boolean,
    ownerSignal?: AbortSignal,
    requestedRole?: ConnectionRole,
  ): Promise<void> {
    if (!store.serverCatalog)
      throw new Error("The server catalogue is unavailable");
    if (store.unresolvedServers.length || !store.selectedServers.length)
      throw new Error("Review the saved server selection");
    if (!force && readySelected()) return;
    validation?.abort.abort();
    const task = {
      abort: new AbortController(),
      draft: draftKey(store.config),
      roles:
        store.selectedServers.length === 1
          ? validationRoles(
              store.config,
              store.connectionValidation,
              requestedRole,
              store.transportDiscovery,
            )
          : CONNECTION_ROLES,
    };
    validation = task;
    const signal = AbortSignal.any([
      task.abort.signal,
      ...(ownerSignal ? [ownerSignal] : []),
      AbortSignal.timeout(12_000),
    ]);
    mark(task.roles, "checking");
    try {
      await Promise.allSettled(
        catalogSelected().map((server) =>
          inspectServer(server, signal, task.roles),
        ),
      );
      signal.throwIfAborted();
      if (validation !== task || task.draft !== draftKey(store.config))
        throw new DOMException("Aborted", "AbortError");
      adoptSelectedEvidence();
      if (!readySelected())
        throw new Error(
          store.selectedServers.length === 1
            ? store.serverReadiness[store.selectedServers[0]]?.message ||
                "Could not connect to this server"
            : "Resolve the selected servers before starting",
        );
      failures = 0;
      nextValidationAt = Date.now() + CONNECTION_FRESH_MS;
    } finally {
      if (validation === task) {
        validation = null;
        if (task.draft !== draftKey(store.config))
          nextValidationAt = Date.now();
        else if (!readySelected())
          nextValidationAt = Date.now() + connectionFailureBackoff(++failures);
      }
      schedule();
    }
  }
  function openServers() {
    if (store.isRunning || store.preparing) return;
    store.serverChooserOpen = true;
    if (!store.serverCatalog) {
      void retryCatalogue();
      return;
    }
    inspection?.abort();
    inspection = new AbortController();
    const signal = AbortSignal.any([
      inspection.signal,
      AbortSignal.timeout(12_000),
    ]);
    const pending = (store.serverCatalog?.servers ?? []).filter(
      (server) =>
        store.serverReadiness[server.id]?.state !== "checking" &&
        Date.now() - (store.serverReadiness[server.id]?.checkedAt ?? 0) >
          CONNECTION_FRESH_MS,
    );
    // Four bounded discovery tasks at a time; opening the chooser creates no persistent fleet monitor.
    const worker = async () => {
      while (pending.length && !signal.aborted) {
        const server = pending.shift()!;
        await inspectServer(server, signal).catch(() => {});
      }
    };
    void Promise.all([worker(), worker(), worker(), worker()]);
  }
  function closeServers() {
    store.serverChooserOpen = false;
    inspection?.abort();
    inspection = null;
  }
  function applyServers(ids: string[]): boolean {
    if (store.isRunning || store.preparing || !store.serverCatalog)
      return false;
    validateSelection(store.serverCatalog, ids);
    cancelPendingStart();
    validation?.abort.abort();
    store.selectedServers = selectedInCatalogOrder(
      store.serverCatalog,
      ids,
    ).map((server) => server.id);
    store.unresolvedServers = [];
    try {
      localStorage.setItem(
        savedSelectionKey,
        JSON.stringify(catalogSelected().map(({ id, url }) => ({ id, url }))),
      );
    } catch {}
    closeServers();
    mark(CONNECTION_ROLES, "stale");
    requestValidation();
    return true;
  }
  async function signInServer(id: string) {
    const server = store.serverCatalog?.servers.find(
      (server) => server.id === id,
    );
    if (!server || store.isRunning || store.preparing) return;
    approval?.abort();
    approval = new AbortController();
    const task = approval;
    // Create the browsing context during the click. The visible URL remains a fallback when popups are blocked.
    const popup = window.open(
      "about:blank",
      "_blank",
      "popup,width=520,height=720",
    );
    try {
      const flow = await browserApproval(server);
      task.signal.throwIfAborted();
      store.serverApproval = { id, url: flow.url };
      if (popup) popup.location.replace(flow.url);
      const context = await flow.poll(task.signal);
      if (approval !== task) return;
      contexts.set(id, context);
      store.serverApproval = null;
      await inspectServer(
        server,
        AbortSignal.any([task.signal, AbortSignal.timeout(12_000)]),
      );
      if (store.selectedServers.includes(id)) adoptSelectedEvidence();
    } catch (cause) {
      if (!task.signal.aborted) {
        store.serverReadiness[id] = {
          state: "sign-in",
          message: cause instanceof Error ? cause.message : "Approval failed",
        };
        if (store.serverApproval)
          store.serverApproval = {
            ...store.serverApproval,
            message: "Approval did not finish. Try Sign in again.",
          };
      }
    }
  }
  function configureLatency(
    mode: "primary" | "all",
    serverId = store.primaryLatencyServer,
  ) {
    if (
      store.isRunning ||
      store.preparing ||
      !store.selectedServers.includes(serverId)
    )
      return false;
    store.latencySelection = { mode, serverId };
    return true;
  }
  function focusServer(id: string) {
    if (
      store.serverDetails &&
      !store.serverDetails.servers.some(
        (server) => server.server.id === id && server.latencyTarget,
      )
    )
      return;
    store.focusLatencyServer(id);
    runner?.focusServer?.(id);
  }
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
    if (useCatalog) return validateServers(force, ownerSignal, requestedRole);
    if (!force && freshPaths()) return;
    const config = $state.snapshot(store.config);
    const draft = draftKey(config);
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
      draftKey(store.config) === draft;
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

  function onAuthenticationRequired(event: Event) {
    if (!booted) return;
    const reason =
      event instanceof CustomEvent && event.detail === "renew"
        ? "renew"
        : "expired";
    dispose();
    location.replace(`/login?reason=${reason}`);
  }

  async function boot() {
    if (booted) return;
    booted = true;
    lifetime = new AbortController();
    const ownLifetime = lifetime;
    window.addEventListener(
      AUTHENTICATION_REQUIRED_EVENT,
      onAuthenticationRequired,
    );
    store.engineInfo = describe();
    online = typeof navigator === "undefined" ? null : navigator.onLine;
    if (useCatalog)
      await loadCatalog().catch((cause) => {
        store.startError =
          cause instanceof Error ? cause.message : "Could not load servers";
      });
    if (!booted || ownLifetime.signal.aborted) return;
    let serverDraft = selectionKey();
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
        const selectionChanged = serverDraft !== selectionKey();
        serverDraft = selectionKey();
        if (!booted || (!changed.length && !selectionChanged)) return;
        if (useCatalog) {
          for (const id of store.selectedServers) {
            store.serverReadiness[id] = { state: "unchecked" };
            selectedPaths.delete(id);
          }
          if (pendingStart) cancelPendingStart();
          validation?.abort.abort();
          if (!store.isRunning) requestValidation();
          return;
        }
        const draft = draftKey(store.config);
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
    if (
      useCatalog &&
      store.unresolvedServers.length &&
      store.serverCatalog?.servers.length === 1
    ) {
      store.startError =
        "The saved selection changed. Open Settings to use this server.";
      return;
    }
    if (
      useCatalog &&
      (!store.serverCatalog ||
        store.unresolvedServers.length ||
        (!readySelected() && (store.serverCatalog?.servers.length ?? 0) > 1))
    ) {
      openServers();
      store.startError = "Resolve the selected servers before starting.";
      return;
    }
    const config = $state.snapshot(store.config);
    config.adaptive = canonicalAdaptiveConfig(config.adaptive);
    const abort = new AbortController();
    pendingStart = { abort, draft: draftKey(config) };
    store.startError = "";
    store.preparationStatus = "authenticating";
    const current = () =>
      booted &&
      pendingStart?.abort === abort &&
      !abort.signal.aborted &&
      draftKey(store.config) === draftKey(config);
    const start = async () => {
      const budget = await requireSessionCoverage(
        buildSegments(config).totalMs + SESSION_RUN_MARGIN_MS,
        abort.signal,
      );
      if (!current()) return;
      sessionBudget = budget;
      if (useCatalog)
        for (const server of catalogSelected()) {
          const context = contexts.get(server.id);
          if (context?.kind !== "grant") continue;
          const remainingMs = (context.expiresAt ?? 0) - Date.now();
          if (
            remainingMs <
            buildSegments(config).totalMs + SESSION_RUN_MARGIN_MS
          ) {
            store.serverReadiness[server.id] = {
              state: "sign-in",
              message: "Sign in again to cover the planned test duration",
            };
            throw new SessionCoverageError(
              `${server.name}: sign in again to cover the planned test duration`,
            );
          }
          if (!sessionBudget || remainingMs < sessionBudget.remainingMs)
            sessionBudget = {
              remainingMs,
              maximumLifetimeMs: remainingMs,
              checkedAt: performance.now(),
            };
        }
      store.reset();
      store.preparationStatus = "checking";
      await validateConnections(false, undefined, abort.signal);
      if (!current()) return;
      const paths = freshPaths();
      if (!paths) throw new Error("connection evidence expired before start");
      store.preparationStatus = "launching";
      unsubscribe?.();
      runner?.dispose();
      if (useCatalog) {
        const selected = catalogSelected();
        const prepared = selected.map((server) => ({
          server,
          paths: selectedPaths.get(server.id)!,
        }));
        const focus =
          store.latencySelection.mode === "primary"
            ? prepared.find(
                (server) => server.server.id === store.primaryLatencyServer,
              )!
            : prepared.reduce(
                (best, next) =>
                  (next.paths.latency?.rttMs ?? Infinity) <
                  (best.paths.latency?.rttMs ?? Infinity)
                    ? next
                    : best,
                prepared[0],
              );
        store.latencyFocus = focus.server.id;
        for (const monitor of selectedIdle.values()) monitor.stop();
        runner = new ServerCoordinator(prepared, focus.server.id);
      } else runner = createRunner(paths);
      unsubscribe = runner.on(ingest);
      store.activeConfig = structuredClone(config);
      store.activePaths = paths;
      if (runner instanceof ServerCoordinator)
        store.serverDetails = runner.details();
      idle?.stop();
      runner.start(config, paths.latency?.rttMs ?? 0);
    };
    void start()
      .catch((cause) => {
        if (!current()) return;
        if (cause instanceof DOMException && cause.name === "AbortError")
          return;
        store.startError =
          cause instanceof SessionCoverageError ||
          (useCatalog && cause instanceof Error)
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
    if (store.preparing) return false;
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
    lifetime.abort();
    catalogCheck?.abort();
    inspection?.abort();
    approval?.abort();
    for (const monitor of selectedIdle.values()) monitor.stop();
    contexts.clear();
    selectedPaths.clear();
    selectedIdle.clear();
    selectedValidation.clear();
    for (const check of serverChecks.values()) check.abort();
    serverChecks.clear();
    window.removeEventListener(
      AUTHENTICATION_REQUIRED_EVENT,
      onAuthenticationRequired,
    );
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
    openServers,
    closeServers,
    retryCatalogue,
    applyServers,
    signInServer,
    focusServer,
    configureLatency,
    retryServer: (id: string) => {
      const server = store.serverCatalog?.servers.find(
        (server) => server.id === id,
      );
      return server
        ? inspectServer(server, AbortSignal.timeout(12_000)).catch(() => {})
        : Promise.resolve();
    },
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
