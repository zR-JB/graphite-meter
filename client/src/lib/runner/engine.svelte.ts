import { readJSONResponse } from "../api/decode";
import { authenticatedFetch } from "../auth";
import {
  parseCatalog,
  reconcileSelection,
  selectedInCatalogOrder,
  validateSelection,
  type ServerCatalog,
  type ServerEntry,
} from "../servers/catalog";
import {
  browserApproval,
  BrowserApprovalLimitError,
  serverCredentials,
  ServerAuthenticationRequired,
  type ServerCredentials,
} from "../servers/credentials";
import { ServerCoordinator, type PreparedServer } from "../servers/coordinator";
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
import {
  BrowserOriginBlockedError,
  PreflightUnavailableError,
  TransportUnavailableError,
} from "./real/transportError";
import {
  discoverServer,
  prepareConnections,
  type ConnectionPreparation,
} from "./real/prepare";
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
  if (cause instanceof BrowserOriginBlockedError) return cause.message;
  return cause instanceof PreflightUnavailableError &&
    isNetworkUnavailable(cause.cause)
    ? "Server could not be reached"
    : "Connection check failed";
}

interface ApplicationDependencies {
  loadCatalog: (signal: AbortSignal) => Promise<ServerCatalog>;
  discover: typeof discoverServer;
  prepare: typeof prepareConnections;
  createRunner: (servers: PreparedServer[], focus: string) => NetworkRunner;
  describe: () => EngineInfo;
}

async function loadServerCatalog(signal: AbortSignal): Promise<ServerCatalog> {
  const response = await authenticatedFetch("/servers", {
    cache: "no-store",
    signal,
  });
  if (!response.ok) throw new Error("Could not load the server catalogue");
  return parseCatalog(await readJSONResponse(response), location.origin);
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
  const fetchCatalog =
    dependencies.loadCatalog ??
    (dummy ? DummyBackend.loadCatalog : loadServerCatalog);
  const discover =
    dependencies.discover ?? (dummy ? DummyBackend.discover : discoverServer);
  const createRunner =
    dependencies.createRunner ??
    ((servers: PreparedServer[], focus: string) =>
      dummy
        ? new RunnerCore(new DummyBackend())
        : new ServerCoordinator(servers, focus));
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
  let metadataCheck: AbortController | null = null;
  const discoveryRetryAt = new Map<string, number>();
  let sessionBudget: SessionBudget | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let nextValidationAt = 0;
  let failures = 0;
  let online: boolean | null = null;
  const SESSION_RUN_MARGIN_MS = 60_000;

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
  let approval: AbortController | null = null;
  let approvalServerId: string | null = null;
  let approvalPopup: Window | null = null;
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
    JSON.stringify([connectionDraftKey(config), selectionKey()]);
  const catalogSelected = () =>
    selectedInCatalogOrder(store.serverCatalog!, store.selectedServers);
  const readySelected = (fresh = true) =>
    store.serverCatalog &&
    !store.unresolvedServers.length &&
    store.selectedServers.length > 0 &&
    store.selectedServers.every(
      (id) =>
        store.serverReadiness.get(id)?.state === "ready" &&
        selectedPaths.has(id) &&
        !!preparedPaths(
          serverConfig(id),
          store.serverDiscoveries.get(id) ?? null,
          selectedValidation.get(id) ?? emptyConnectionValidation(),
          fresh ? CONNECTION_FRESH_MS : Infinity,
        ),
    );
  function updateSelectedPaths() {
    for (const id of store.selectedServers) {
      const config = serverConfig(id);
      if (!latencyPathNeeded(config)) {
        selectedIdle.get(id)?.stop();
      }
      const paths = preparedPaths(
        config,
        store.serverDiscoveries.get(id) ?? null,
        selectedValidation.get(id) ?? emptyConnectionValidation(),
        Infinity,
      );
      if (
        paths &&
        (config.stages.upload || config.stages.bidirectional) &&
        !paths.discovery.uploadCheckpoint
      ) {
        selectedPaths.delete(id);
        store.serverReadiness.set(id, {
          state: "failed",
          message:
            "Receiver checkpoint support is required for uploads. Upgrade this measurement server.",
        });
        continue;
      }
      if (paths) {
        paths.credentials = contexts.get(id);
        selectedPaths.set(id, paths);
        store.serverReadiness.set(id, {
          ...store.serverReadiness.get(id),
          state: "ready",
        });
      } else {
        selectedPaths.delete(id);
        if (
          !["failed", "sign-in"].includes(
            store.serverReadiness.get(id)?.state ?? "unchecked",
          )
        )
          store.serverReadiness.set(id, { state: "unchecked" });
      }
    }
  }
  function invalidateSelected(
    roles: ConnectionRole[],
    ids = store.selectedServers,
  ) {
    for (const id of ids) {
      const previous = selectedValidation.get(id);
      if (previous) {
        const next = { ...previous };
        if (roles.includes("throughput"))
          next.throughput = { ...next.throughput, state: "stale" };
        if (roles.includes("latency"))
          next.latency = { ...next.latency, state: "stale" };
        selectedValidation.set(id, next);
      }
      selectedPaths.delete(id);
      store.serverReadiness.set(id, { state: "unchecked" });
    }
  }
  const savedSelectionKey = "graphite-meter:server-selection:v1";

  function freshDiscovery(id: string) {
    const discovery = store.serverDiscoveries.get(id);
    return discovery && Date.now() - discovery.fetchedAt <= CONNECTION_FRESH_MS
      ? discovery
      : undefined;
  }
  async function serverDiscovery(
    server: ServerEntry,
    signal: AbortSignal,
    force = false,
  ) {
    const cached = !force && freshDiscovery(server.id);
    if (cached) return cached;
    const catalog = store.serverCatalog;
    try {
      const discovery = await discover(signal, contexts.get(server.id));
      signal.throwIfAborted();
      if (catalog !== store.serverCatalog)
        throw new DOMException("Aborted", "AbortError");
      // Metadata and path checks share discovery; only role probes establish readiness.
      store.serverDiscoveries.set(server.id, discovery);
      server.name = discovery.server.name || server.name;
      server.location = discovery.server.location;
      const context = contexts.get(server.id);
      if (context)
        context.server = {
          ...context.server,
          name: server.name,
          location: server.location,
        };
      discoveryRetryAt.delete(server.id);
      return discovery;
    } catch (cause) {
      if (!signal.aborted || signal.reason?.name === "TimeoutError")
        discoveryRetryAt.set(
          server.id,
          Date.now() + connectionFailureBackoff(1),
        );
      throw cause;
    }
  }
  function cancelServerMetadata() {
    metadataCheck?.abort();
    metadataCheck = null;
    store.serverMetadataLoading = false;
  }
  function loadServerMetadata() {
    const catalog = store.serverCatalog;
    if (
      !booted ||
      !catalog ||
      store.isRunning ||
      store.preparing ||
      store.selectionValidation === "checking"
    )
      return;
    cancelServerMetadata();
    const now = Date.now();
    const pending = catalog.servers.filter(
      (server) =>
        !store.selectedServers.includes(server.id) &&
        now >= (discoveryRetryAt.get(server.id) ?? 0) &&
        !freshDiscovery(server.id),
    );
    if (!pending.length) return;
    const task = new AbortController();
    metadataCheck = task;
    store.serverMetadataLoading = true;
    const budget = AbortSignal.any([
      task.signal,
      lifetime.signal,
      AbortSignal.timeout(10_000),
    ]);
    const refresh = async () => {
      // Discovery is serial and only runs while the Settings request is current.
      for (const server of pending) {
        if (budget.aborted) break;
        if (store.selectedServers.includes(server.id)) continue;
        try {
          await serverDiscovery(
            server,
            AbortSignal.any([budget, AbortSignal.timeout(5000)]),
          );
        } catch {
          if (budget.aborted) return;
        }
      }
    };
    void refresh().finally(() => {
      if (metadataCheck === task) {
        metadataCheck = null;
        store.serverMetadataLoading = false;
      }
    });
  }

  async function loadCatalog() {
    cancelServerMetadata();
    discoveryRetryAt.clear();
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
      const catalog = await fetchCatalog(signal);
      signal.throwIfAborted();
      validation?.abort.abort();
      for (const check of serverChecks.values()) check.abort();
      serverChecks.clear();
      const previousContexts = new Map(contexts);
      contexts.clear();
      selectedPaths.clear();
      selectedValidation.clear();
      for (const monitor of selectedIdle.values()) monitor.stop();
      selectedIdle.clear();
      store.serverDiscoveries.clear();
      store.serverReadiness.clear();
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
        store.serverReadiness.set(server.id, { state: "unchecked" });
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
    roles: ConnectionRole[],
    maxAgeMs: number,
    forceDiscovery = false,
  ): Promise<void> {
    serverChecks.get(server.id)?.abort();
    const check = new AbortController();
    serverChecks.set(server.id, check);
    const signal = AbortSignal.any([
      ownerSignal,
      check.signal,
      lifetime.signal,
      AbortSignal.timeout(12_000),
    ]);
    const configKey = draftKey(store.config);
    const current = () =>
      serverChecks.get(server.id) === check &&
      configKey === draftKey(store.config);
    store.serverReadiness.set(server.id, { state: "checking" });
    const config = serverConfig(server.id);
    let result: ConnectionPreparation | undefined;
    try {
      const discovery = await serverDiscovery(server, signal, forceDiscovery);
      result = await prepare(
        config,
        selectedValidation.get(server.id) ?? emptyConnectionValidation(),
        roles,
        signal,
        contexts.get(server.id),
        discovery,
      );
      signal.throwIfAborted();
      if (!current()) throw new DOMException("Aborted", "AbortError");
      selectedValidation.set(server.id, result.validation);
      if (result.failure) throw result.failure;
      if (
        (config.stages.upload || config.stages.bidirectional) &&
        !result.discovery.uploadCheckpoint
      )
        throw new TransportUnavailableError(
          `${server.name} needs receiver checkpoint support for coordinated uploads. Upgrade this measurement server.`,
          { role: "throughput" },
        );
      const paths = preparedPaths(
        config,
        result.discovery,
        result.validation,
        maxAgeMs,
      );
      if (!paths) throw new Error("Enabled measurements could not be prepared");
      paths.credentials = contexts.get(server.id);
      selectedPaths.set(server.id, paths);
      if (result.idle !== undefined) {
        selectedIdle.get(server.id)?.stop();
        if (result.idle && server.id === "self")
          selectedIdle.set(server.id, result.idle);
        else {
          result.idle?.stop();
          selectedIdle.delete(server.id);
        }
      }
      store.serverReadiness.set(server.id, {
        state: "ready",
        message: `Throughput: ${paths.throughput.target.transport} · ${paths.throughput.target.origin}${paths.latency ? `; latency: ${paths.latency.target.transport} · ${paths.latency.target.origin}` : ""}`,
        checkedAt: Date.now(),
      });
    } catch (cause) {
      result?.idle?.stop();
      if (!current()) throw cause;
      selectedPaths.delete(server.id);
      if (signal.aborted) {
        store.serverReadiness.set(server.id, { state: "unchecked" });
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
      store.serverReadiness.set(server.id, {
        state:
          error instanceof ServerAuthenticationRequired ? "sign-in" : "failed",
        message:
          error instanceof ServerAuthenticationRequired
            ? error.message
            : cause instanceof TransportUnavailableError
              ? cause.message
              : connectionFailureMessage(cause),
        checkedAt: Date.now(),
      });
      throw cause;
    }
  }
  function adoptSelectedEvidence() {
    // Keep the representative paths and discovery in one server generation.
    const selected = catalogSelected();
    const first =
      store.latencySelection.mode === "primary"
        ? selected.find((server) => server.id === store.primaryLatencyServer)!
        : (selected.find((server) => server.id === "self") ?? selected[0]);
    const paths = selectedPaths.get(first.id);
    const checked = selectedValidation.get(first.id);
    const discovery = store.serverDiscoveries.get(first.id);
    if (discovery) store.transportDiscovery = discovery;
    if (checked) store.connectionValidation = checked;
    else
      mark(
        CONNECTION_ROLES,
        "failed",
        "Resolve the selected servers before starting.",
      );
    const nextIdle =
      first.id === "self" && paths?.latency
        ? (selectedIdle.get(first.id) ?? null)
        : null;
    for (const monitor of selectedIdle.values()) {
      if (monitor !== nextIdle) monitor.stop();
    }
    idle = nextIdle;
    if (readySelected(false)) store.connectivity = "connected";
    if (idle) {
      idle.onEvent = (event) => ingest(event, first.id);
      refreshIdle();
    }
  }
  async function validateServers(
    force: boolean,
    ownerSignal?: AbortSignal,
    requestedRole?: ConnectionRole,
    refreshDiscoveryId?: string,
  ): Promise<void> {
    if (!store.serverCatalog)
      throw new Error("The server catalogue is unavailable");
    if (store.unresolvedServers.length || !store.selectedServers.length)
      throw new Error("Review the saved server selection");
    const requireFresh = ownerSignal !== undefined;
    if (!force && readySelected(requireFresh)) {
      adoptSelectedEvidence();
      return;
    }
    const pending = catalogSelected().flatMap((server) => {
      const roles = validationRoles(
        serverConfig(server.id),
        selectedValidation.get(server.id) ?? emptyConnectionValidation(),
        force ? (requestedRole ?? "all") : undefined,
        store.serverDiscoveries.get(server.id),
        requireFresh ? CONNECTION_FRESH_MS : Infinity,
      );
      return roles.length ? [{ server, roles }] : [];
    });
    if (!pending.length) {
      updateSelectedPaths();
      adoptSelectedEvidence();
      if (!readySelected(requireFresh))
        throw new Error("Resolve the selected servers before starting");
      return;
    }
    cancelServerMetadata();
    validation?.abort.abort();
    const task = {
      abort: new AbortController(),
      draft: draftKey(store.config),
      roles: [...new Set(pending.flatMap(({ roles }) => roles))],
    };
    validation = task;
    const signal = AbortSignal.any([
      task.abort.signal,
      ...(ownerSignal ? [ownerSignal] : []),
    ]);
    mark(task.roles, "checking");
    try {
      // Bound simultaneous handshakes; each server retains its own deadline.
      const worker = async () => {
        while (pending.length && !signal.aborted) {
          const { server, roles } = pending.shift()!;
          await inspectServer(
            server,
            signal,
            roles,
            requireFresh ? CONNECTION_FRESH_MS : Infinity,
            force || refreshDiscoveryId === server.id,
          ).catch(() => {});
        }
      };
      await Promise.all([worker(), worker()]);
      signal.throwIfAborted();
      if (validation !== task || task.draft !== draftKey(store.config))
        throw new DOMException("Aborted", "AbortError");
      adoptSelectedEvidence();
      if (!readySelected(requireFresh))
        throw new Error(
          store.selectedServers.length === 1
            ? store.serverReadiness.get(store.selectedServers[0])?.message ||
                "Could not connect to this server"
            : "Resolve the selected servers before starting",
        );
      failures = 0;
      nextValidationAt = Date.now() + CONNECTION_FRESH_MS;
    } finally {
      if (validation === task) {
        validation = null;
        if (task.abort.signal.aborted || task.draft !== draftKey(store.config))
          nextValidationAt = Date.now();
        else if (!readySelected(requireFresh))
          nextValidationAt = Date.now() + connectionFailureBackoff(++failures);
      }
      schedule();
    }
  }
  function applyServers(ids: string[]): boolean {
    if (store.isRunning || store.preparing || !store.serverCatalog)
      return false;
    validateSelection(store.serverCatalog, ids);
    cancelServerMetadata();
    if (approvalServerId && !ids.includes(approvalServerId))
      cancelServerApproval();
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
    return true;
  }
  function cancelServerApproval() {
    approval?.abort();
    approval = null;
    approvalServerId = null;
    store.serverApproval = null;
    approvalPopup?.close();
    approvalPopup = null;
  }
  async function signInServer(id: string) {
    const server = store.serverCatalog?.servers.find(
      (server) => server.id === id,
    );
    if (!server || store.isRunning || store.preparing) return;
    cancelServerApproval();
    approval = new AbortController();
    approvalServerId = id;
    const task = approval;
    // Create the browsing context during the click. The visible URL remains a fallback when popups are blocked.
    const popup = (approvalPopup = window.open(
      "about:blank",
      "_blank",
      "popup,width=520,height=720",
    ));
    try {
      if (popup) popup.opener = null;
      const flow = await browserApproval(server);
      task.signal.throwIfAborted();
      store.serverApproval = { id, url: flow.url, code: flow.code };
      if (popup) popup.location.replace(flow.url);
      const context = await flow.poll(task.signal);
      if (approval !== task) return;
      contexts.set(id, context);
      store.serverDiscoveries.delete(id);
      discoveryRetryAt.delete(id);
      selectedPaths.delete(id);
      selectedValidation.delete(id);
      store.serverReadiness.set(id, { state: "unchecked" });
      store.serverApproval = null;
      // Approval owns the grant exchange; connection validation owns path errors
      // and superseding draft changes after the grant has been accepted.
      void validateServers(false).catch(() => {});
    } catch (cause) {
      if (!task.signal.aborted) {
        store.serverReadiness.set(id, {
          state: "sign-in",
          message: cause instanceof Error ? cause.message : "Approval failed",
        });
        if (store.serverApproval)
          store.serverApproval = {
            ...store.serverApproval,
            message:
              cause instanceof BrowserApprovalLimitError
                ? "Renew the remote login, then choose Sign in again."
                : "Approval did not finish. Try Sign in again.",
            renewUrl:
              cause instanceof BrowserApprovalLimitError
                ? `${server.url}/login`
                : undefined,
          };
      }
    } finally {
      if (approval === task) {
        popup?.close();
        approval = null;
        approvalServerId = null;
        approvalPopup = null;
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
    // Healthy idle paths need no periodic handshakes. Expiry is checked on start.
    if (readySelected(false)) return;
    timer = setTimeout(
      () => {
        timer = undefined;
        void validateConnections().catch(() => {});
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
  function ingest(event: RunnerEvent, serverId?: string) {
    if (event.type === "serverFailure") {
      invalidateSelected([event.failure.scope], [event.failure.serverId]);
      nextValidationAt = Date.now();
    }
    if (event.type === "connectivity") {
      if (event.state === "offline") offline(serverId);
      else onlineAgain();
    }
    if (
      event.type === "error" &&
      CONNECTION_FAILURE_REASONS.has(event.error.reason)
    ) {
      invalidateSelected(CONNECTION_ROLES);
      mark(CONNECTION_ROLES, "stale", "Connection changed; check again.");
    }
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
  function offline(serverId?: string | Event) {
    if (online === false && !(serverId instanceof Event)) return;
    online = false;
    const roles: ConnectionRole[] =
      typeof serverId === "string" ? ["latency"] : CONNECTION_ROLES;
    invalidateSelected(
      roles,
      typeof serverId === "string" ? [serverId] : undefined,
    );
    mark(roles, "stale", "Connection changed; check again.");
    requestValidation();
  }
  function onlineAgain() {
    if (online === true && readySelected(false)) return;
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
    return validateServers(force, ownerSignal, requestedRole);
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
    await loadCatalog().catch((cause) => {
      if (ownLifetime.signal.aborted) return;
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
        draftKeys = keys;
        const selectionChanged = serverDraft !== selectionKey();
        serverDraft = selectionKey();
        if (!booted || (!changed.length && !selectionChanged)) return;
        if (pendingStart) cancelPendingStart();
        validation?.abort.abort();
        updateSelectedPaths();
        if (store.selectedServers.length) adoptSelectedEvidence();
        if (!store.isRunning) requestValidation();
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
    cancelServerMetadata();
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
      store.unresolvedServers.length &&
      store.serverCatalog?.servers.length === 1
    ) {
      store.startError =
        "The saved selection changed. Open Settings to use this server.";
      return;
    }
    if (
      !store.serverCatalog ||
      store.unresolvedServers.length ||
      (!readySelected(false) && (store.serverCatalog?.servers.length ?? 0) > 1)
    ) {
      store.startError =
        "Open Settings to resolve the selected servers before starting.";
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
      for (const server of catalogSelected()) {
        const context = contexts.get(server.id);
        if (context?.kind !== "grant") continue;
        const remainingMs = (context.expiresAt ?? 0) - Date.now();
        if (
          remainingMs <
          buildSegments(config).totalMs + SESSION_RUN_MARGIN_MS
        ) {
          store.serverReadiness.set(server.id, {
            state: "sign-in",
            message: "Sign in again to cover the planned test duration",
          });
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
      const prepared = catalogSelected().map((server) => ({
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
      const paths = focus.paths;
      store.preparationStatus = "launching";
      store.latencyFocus = focus.server.id;
      unsubscribe?.();
      runner?.dispose();
      for (const monitor of selectedIdle.values()) monitor.stop();
      runner = createRunner(prepared, focus.server.id);
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
          cause instanceof Error
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
    if (store.isRunning) {
      try {
        runner?.reconfigure(live);
      } catch (cause) {
        store.startError =
          cause instanceof Error
            ? cause.message
            : "This change cannot be applied to the active test.";
        return false;
      }
    }
    cancelPendingStart();
    store.config = config;
    store.startError = "";
    if (!store.isRunning) return true;
    store.compactThroughputForDuration(candidateTotal);
    if (store.activeConfig)
      store.activeConfig = { ...store.activeConfig, ...live };
    return true;
  }

  function dispose() {
    booted = false;
    lifetime.abort();
    cancelServerMetadata();
    discoveryRetryAt.clear();
    catalogCheck?.abort();
    cancelServerApproval();
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
    retryCatalogue,
    loadServerMetadata,
    cancelServerMetadata,
    applyServers,
    signInServer,
    cancelServerApproval,
    focusServer,
    configureLatency,
    retryServer: (id: string) => {
      const server = store.serverCatalog?.servers.find(
        (server) => server.id === id,
      );
      if (
        !server ||
        !store.selectedServers.includes(id) ||
        store.isRunning ||
        store.preparing
      )
        return Promise.resolve();
      // Capability failures can leave both role probes verified. A manual retry
      // must rediscover that server after an upgrade, without checking its peers.
      if (
        store.serverReadiness.get(id)?.state === "failed" &&
        !validationRoles(
          serverConfig(id),
          selectedValidation.get(id) ?? emptyConnectionValidation(),
          undefined,
          store.serverDiscoveries.get(id),
          Infinity,
        ).length
      )
        selectedValidation.delete(id);
      return validateServers(false, undefined, undefined, id).catch(() => {});
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
