import { untrack } from "svelte";
import {
  ServerConnections,
  connectionFailureMessage,
  type ServerConnectionView,
} from "../servers/connections";
import { readJSONResponse } from "../api/decode";
import { authenticatedFetch } from "../auth";
import {
  parseCatalog,
  allowsServerOrigin,
  reconcileSelection,
  selectedInCatalogOrder,
  validateSelection,
  type ServerCatalog,
} from "../servers/catalog";
import {
  browserApproval,
  BrowserApprovalLimitError,
  openBrowserApprovalPopup,
} from "../servers/credentials";
import { ServerCoordinator, type PreparedServer } from "../servers/coordinator";
import { portableTransportSelection } from "../servers/transportOptions";
import type {
  ConnectionRole,
  EngineInfo,
  LiveRunConfig,
  NetworkRunner,
  RunnerEvent,
} from "./contract";
import { RunnerCore } from "./core";
import { DummyBackend } from "./dummy";
import { RealBackend } from "./RealRunner";
import { discoverServer, prepareConnections } from "./real/prepare";
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
  CONNECTION_ROLES,
  connectionDraftKey,
  connectionDraftRoleKey,
  emptyConnectionValidation,
  latencyPathNeeded,
  uploadCapabilityFailure,
} from "./connectionModel";

export { connectionFailureMessage } from "../servers/connections";

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
  let activeCapabilities: { name: string; uploadCheckpoint?: boolean }[] = [];
  let unsubscribe: (() => void) | undefined;
  let disposeDraft: (() => void) | undefined;
  let pendingStart: { abort: AbortController; draft: string } | null = null;
  let booted = false;
  let lifetime = new AbortController();
  let catalogCheck: AbortController | null = null;
  let sessionBudget: SessionBudget | null = null;
  const SESSION_RUN_MARGIN_MS = 60_000;
  const connections = new ServerConnections({
    discover,
    prepare,
    changed: publishConnection,
    idleEvent: (id, event) => ingest(event, id),
  });
  let approval: AbortController | null = null;
  let approvalServerId: string | null = null;
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
  function makeTransportPortable(role: ConnectionRole) {
    const key = role === "throughput" ? "throughputTarget" : "latencyTarget";
    store.config.transports[key] = portableTransportSelection(
      role,
      store.config.transports[key],
      store.transportDiscovery,
    );
  }
  const readySelected = (fresh = true, ids = store.selectedServers) =>
    !!store.serverCatalog &&
    !store.unresolvedServers.length &&
    connections.ready(ids, fresh ? CONNECTION_FRESH_MS : Infinity);
  function syncIntent() {
    connections.select(
      store.serverCatalog
        ? store.selectedServers.map((id) => ({ id, config: serverConfig(id) }))
        : [],
    );
    adoptSelectedEvidence();
  }
  function invalidateSelected(
    roles: ConnectionRole[],
    ids = store.selectedServers,
  ) {
    connections.invalidate(ids, roles);
  }
  function publishConnection(view: ServerConnectionView) {
    store.serverReadiness.set(view.server.id, view.readiness);
    store.serverValidation.set(view.server.id, view.validation);
    if (view.discovery)
      store.serverDiscoveries.set(view.server.id, view.discovery);
    else store.serverDiscoveries.delete(view.server.id);
    const server = store.serverCatalog?.servers.find(
      (server) => server.id === view.server.id,
    );
    if (server) {
      server.name = view.server.name;
      server.location = view.server.location;
    }
    store.serverMetadataLoading = connections.metadataLoading;
    adoptSelectedEvidence();
  }
  const savedSelectionKey = "graphite-meter:server-selection:v1";

  function cancelServerMetadata() {
    connections.metadata(false);
    store.serverMetadataLoading = false;
  }
  function loadServerMetadata() {
    if (!booted || !store.serverCatalog || store.isRunning || store.preparing)
      return;
    connections.metadata(true);
    schedule();
  }

  async function loadCatalog() {
    cancelServerMetadata();
    cancelServerApproval();
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
      store.serverDiscoveries.clear();
      store.serverValidation.clear();
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
      // Old saved configurations may still pin a single origin while selecting
      // several servers. A shared preference must resolve within each server.
      for (const role of CONNECTION_ROLES) {
        const selected = catalog.servers.filter(
          (server) =>
            selection.ids.includes(server.id) &&
            (role !== "latency" ||
              store.latencySelection.mode === "all" ||
              server.id === store.primaryLatencyServer),
        );
        const value =
          store.config.transports[
            role === "throughput" ? "throughputTarget" : "latencyTarget"
          ];
        if (
          selected.length > 1 ||
          (value !== "auto" &&
            !value.startsWith("protocol:") &&
            !value.startsWith("transport:") &&
            selected.some(
              (server) =>
                !allowsServerOrigin(
                  server,
                  value.replace(/::(?:wt|wtdg)$/, ""),
                ),
            ))
        )
          makeTransportPortable(role);
      }
      store.transportDiscovery = null;
      store.connectionValidation = emptyConnectionValidation();
      connections.reset(catalog.servers);
      syncIntent();
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
  function representativeServer() {
    if (!store.serverCatalog || !store.selectedServers.length) return null;
    const selected = catalogSelected();
    return store.latencySelection.mode === "primary"
      ? (selected.find((server) => server.id === store.primaryLatencyServer) ??
          selected[0])
      : (selected.find((server) => server.id === "self") ?? selected[0]);
  }
  function adoptSelectedEvidence() {
    const first = representativeServer();
    store.transportDiscovery = first
      ? (store.serverDiscoveries.get(first.id) ?? null)
      : null;
    store.connectionValidation = first
      ? (store.serverValidation.get(first.id) ?? emptyConnectionValidation())
      : emptyConnectionValidation();
    if (readySelected(false)) store.connectivity = "connected";
    refreshIdle();
  }
  async function validateServers(
    force: boolean,
    ownerSignal?: AbortSignal,
    requestedRole?: ConnectionRole,
    serverId?: string,
  ): Promise<void> {
    if (!store.serverCatalog)
      throw new Error("The server catalogue is unavailable");
    if (store.unresolvedServers.length || !store.selectedServers.length)
      throw new Error("Review the saved server selection");
    syncIntent();
    await connections.check({
      ids: serverId ? [serverId] : [...store.selectedServers],
      force,
      role: requestedRole,
      fresh: ownerSignal !== undefined,
      signal: ownerSignal,
    });
  }
  function applyServers(ids: string[]): boolean {
    if (store.isRunning || store.preparing || !store.serverCatalog)
      return false;
    validateSelection(store.serverCatalog, ids);
    const next = selectedInCatalogOrder(store.serverCatalog, ids).map(
      (server) => server.id,
    );
    const changed =
      JSON.stringify(next) !== JSON.stringify(store.selectedServers);
    if (changed) {
      makeTransportPortable("throughput");
      if (
        store.latencySelection.mode === "all" ||
        !next.includes(store.primaryLatencyServer)
      )
        makeTransportPortable("latency");
    }
    if (approvalServerId && !ids.includes(approvalServerId))
      cancelServerApproval();
    cancelPendingStart();
    store.selectedServers = next;
    store.unresolvedServers = [];
    requestValidation();
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
  }
  async function signInServer(id: string) {
    const server = store.serverCatalog?.servers.find(
      (server) => server.id === id,
    );
    if (
      !server ||
      !store.selectedServers.includes(id) ||
      store.catalogLoading ||
      store.isRunning ||
      store.preparing
    )
      return;
    cancelServerApproval();
    approval = new AbortController();
    approvalServerId = id;
    const task = approval;
    const popup = openBrowserApprovalPopup(task.signal);
    try {
      const flow = await browserApproval(server);
      task.signal.throwIfAborted();
      store.serverApproval = { id, url: flow.url, code: flow.code };
      popup.navigate(flow.url);
      const context = await flow.poll(task.signal);
      if (approval !== task) return;
      connections.authorize(context);
      store.serverApproval = null;
      // Approval owns the grant exchange; connection validation owns path errors
      // and superseding draft changes after the grant has been accepted.
      void validateServers(false).catch(() => {});
    } catch (cause) {
      if (!task.signal.aborted) {
        connections.requireAuthentication(
          id,
          cause instanceof Error ? cause.message : "Approval failed",
        );
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
      popup.close();
      if (approval === task) {
        approval = null;
        approvalServerId = null;
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
    if (
      mode !== store.latencySelection.mode ||
      (mode === "primary" && serverId !== store.primaryLatencyServer)
    )
      makeTransportPortable("latency");
    store.latencySelection = { mode, serverId };
    requestValidation();
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
  function schedule() {
    connections.activity(
      booted &&
        !store.catalogLoading &&
        !store.isRunning &&
        !pendingStart &&
        !hidden(),
      representativeServer()?.id === "self" && latencyPathNeeded(store.config)
        ? "self"
        : null,
    );
  }
  function requestValidation() {
    syncIntent();
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
    schedule();
  }
  function ingest(event: RunnerEvent, serverId?: string) {
    if (event.type === "serverFailure") {
      invalidateSelected([event.failure.scope], [event.failure.serverId]);
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
    const roles: ConnectionRole[] =
      typeof serverId === "string" ? ["latency"] : CONNECTION_ROLES;
    invalidateSelected(
      roles,
      typeof serverId === "string" ? [serverId] : undefined,
    );
    requestValidation();
  }
  function onlineAgain() {
    connections.recover();
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
    if (force && pendingStart) cancelPendingStart();
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
        untrack(() => {
          if (pendingStart && pendingStart.draft !== draftKey(store.config))
            cancelPendingStart();
          syncIntent();
          if (!store.isRunning) schedule();
        });
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
    if (store.catalogLoading || !store.serverCatalog) {
      store.startError = store.catalogLoading
        ? "Servers are still loading. Try again in a moment."
        : "Open Settings to retry loading the server list.";
      store.preparationStatus = "blocked";
      return;
    }
    if (approval) {
      store.startError =
        "Finish signing in to the selected server before starting.";
      store.preparationStatus = "blocked";
      return;
    }
    if (store.unresolvedServers.length || !store.selectedServers.length) {
      store.startError =
        "The saved selection changed. Open Settings to choose the servers to test.";
      store.preparationStatus = "blocked";
      return;
    }
    const config = $state.snapshot(store.config);
    config.adaptive = canonicalAdaptiveConfig(config.adaptive);
    const abort = new AbortController();
    const task = { abort, draft: draftKey(config) };
    pendingStart = task;
    store.startError = "";
    store.preparationStatus = "authenticating";
    const current = () =>
      booted &&
      pendingStart?.abort === abort &&
      !abort.signal.aborted &&
      draftKey(store.config) === task.draft;
    const start = async () => {
      const budget = await requireSessionCoverage(
        buildSegments(config).totalMs + SESSION_RUN_MARGIN_MS,
        abort.signal,
      );
      if (!current()) return;
      sessionBudget = budget;
      for (const server of catalogSelected()) {
        const context = connections.credentials(server.id);
        if (context?.kind !== "grant") continue;
        const remainingMs = (context.expiresAt ?? 0) - Date.now();
        if (
          remainingMs <
          buildSegments(config).totalMs + SESSION_RUN_MARGIN_MS
        ) {
          connections.requireAuthentication(
            server.id,
            "Sign in again to cover the planned test duration",
          );
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
        paths: connections.paths(server.id)!,
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
      connections.activity(false, null);
      activeCapabilities = prepared.map(({ server, paths }) => ({
        name: server.name,
        uploadCheckpoint: paths.discovery.uploadCheckpoint,
      }));
      runner = createRunner(prepared, focus.server.id);
      unsubscribe = runner.on(ingest);
      store.activeConfig = structuredClone(config);
      store.activePaths = paths;
      if (runner instanceof ServerCoordinator)
        store.serverDetails = runner.details();
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
        store.preparationStatus =
          store.preparationStatus === "authenticating" ? "blocked" : "failed";
      })
      .finally(() => {
        if (pendingStart?.abort === abort) {
          pendingStart = null;
          if (store.preparing) store.preparationStatus = "idle";
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
      const unsupported = activeCapabilities.find((capability) =>
        uploadCapabilityFailure(config, capability),
      );
      if (unsupported) {
        store.startError = `${unsupported.name}: ${uploadCapabilityFailure(config, unsupported)}`;
        return false;
      }
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
    if (!store.isRunning) {
      syncIntent();
      schedule();
      return true;
    }
    store.compactThroughputForDuration(candidateTotal);
    if (store.activeConfig)
      store.activeConfig = { ...store.activeConfig, ...live };
    return true;
  }

  function dispose() {
    booted = false;
    lifetime.abort();
    cancelServerMetadata();
    catalogCheck?.abort();
    cancelServerApproval();
    connections.dispose();
    store.serverReadiness.clear();
    store.serverValidation.clear();
    store.serverDiscoveries.clear();
    window.removeEventListener(
      AUTHENTICATION_REQUIRED_EVENT,
      onAuthenticationRequired,
    );
    disposeDraft?.();
    cancelPendingStart();
    unsubscribe?.();
    runner?.dispose();
    runner = null;
    activeCapabilities = [];
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
    syncIntent();
    schedule();
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
      // Explicit retries refresh only this participant, including capabilities.
      return validateServers(true, undefined, undefined, id).catch(() => {});
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
