import { readJSONResponse } from "../api/decode";
import {
  authenticatedFetch,
  requireSessionCoverage,
  AUTHENTICATION_REQUIRED_EVENT,
  liveScheduleFitsSession,
  SessionCoverageError,
  type SessionBudget,
} from "../auth";
import {
  parseCatalog,
  allowsServerOrigin,
  reconcileSelection,
  selectedInCatalogOrder,
  validateSelection,
  type ServerCatalog,
  type ServerEntry,
} from "../servers/catalog";
import {
  browserApproval,
  BrowserApprovalLimitError,
  openBrowserApprovalPopup,
  serverCredentials,
  type ServerCredentials,
} from "../servers/credentials";
import { originLimiter } from "../servers/originLimiter";
import type {
  ConnectionRole,
  LiveRunConfig,
  RunnerConfig,
  RunnerEvent,
} from "./contract";
import { Run, type PreparedServer } from "./run";
import { discoverServer, prepareConnections } from "./real/prepare";
import type {
  store as applicationStore,
  StageKey,
} from "../state/store.svelte";
import { readStored, writeStored } from "../state/persistence";
import { adaptWarmup, buildSegments } from "./schedule";
import {
  CONNECTION_ROLES,
  latencyPathNeeded,
  portableTransportSelection,
  uploadCapabilityFailure,
} from "./paths";
import {
  ServerConnection,
  type CheckOptions,
  type ConnectionHost,
} from "./connection";

interface ApplicationDependencies {
  loadCatalog: (signal: AbortSignal) => Promise<ServerCatalog>;
  discover: ConnectionHost["discover"];
  prepare: ConnectionHost["prepare"];
  createRunner: (servers: PreparedServer[], focus: string) => Runner;
}
export type Runner = Pick<
  Run,
  "start" | "abort" | "dispose" | "on" | "reconfigure" | "details"
>;

const SESSION_RUN_MARGIN_MS = 60_000;
const SAVED_SELECTION_KEY = "graphite-meter:server-selection:v1";
const TARGET_KEY = {
  throughput: "throughputTarget",
  latency: "latencyTarget",
} as const;

async function loadServerCatalog(signal: AbortSignal): Promise<ServerCatalog> {
  const response = await authenticatedFetch("/servers", {
    cache: "no-store",
    signal,
  });
  if (!response.ok) throw new Error("Could not load the server catalog");
  return parseCatalog(await readJSONResponse(response), location.origin);
}

export function createApplicationController(
  store: typeof applicationStore,
  dependencies: Partial<ApplicationDependencies> = {},
) {
  const fetchCatalog = dependencies.loadCatalog ?? loadServerCatalog;
  const createRunner =
    dependencies.createRunner ??
    ((servers: PreparedServer[], focus: string) => new Run(servers, focus));

  const connections = new Map<string, ServerConnection>();
  let metadataWanted = false;
  let runner: Runner | null = null;
  let unsubscribe: (() => void) | undefined;
  let pendingStart: AbortController | null = null;
  let booted = false;
  let lifetime = new AbortController();
  let catalogCheck: AbortController | null = null;
  let sessionBudget: SessionBudget | null = null;
  let approval: { id: string; abort: AbortController } | null = null;
  let runRttMs = 0;
  let idleEvidenceKey = "";

  const hidden = () => document.visibilityState === "hidden";
  const active = () =>
    booted &&
    !store.catalogLoading &&
    !store.isRunning &&
    !pendingStart &&
    !hidden();
  const selected = () =>
    store.serverCatalog
      ? selectedInCatalogOrder(
          store.serverCatalog,
          store.selectedServers,
        ).flatMap((server) => connections.get(server.id) ?? [])
      : [];
  const host: ConnectionHost = {
    discover: dependencies.discover ?? discoverServer,
    prepare: dependencies.prepare ?? prepareConnections,
    limiter: originLimiter(),
    active,
    metadata: () => metadataWanted,
    online: () => navigator.onLine,
    // Only this server's idle latency is shown before a run.
    idle: (id) =>
      id === "self" &&
      active() &&
      store.representativeServerId === id &&
      latencyPathNeeded(store.config),
    publish(view) {
      store.servers.set(view.server.id, view);
      const entry = store.serverCatalog?.servers.find(
        (server) => server.id === view.server.id,
      );
      if (entry && entry.name !== view.server.name)
        entry.name = view.server.name;
      if (entry && entry.location !== view.server.location)
        entry.location = view.server.location;
    },
    idleEvent(id, event) {
      if (event.type === "latency")
        return store.ingest({
          type: "serverLatency",
          serverId: id,
          sample: event.sample,
        });
      store.connectivity = event.state;
      if (event.state === "offline") offline(id);
      else onlineAgain(id);
    },
  };
  const wake = () => {
    for (const connection of connections.values()) connection.wake();
  };
  const check = (options?: CheckOptions) =>
    Promise.all(selected().map((connection) => connection.check(options)));

  function serverConfig(id: string): RunnerConfig {
    const config = $state.snapshot(store.config);
    if (
      store.latencySelection.mode === "primary" &&
      id !== store.primaryLatencyServer
    ) {
      config.stages.latency = false;
      config.skipLoadedLatencyWhenStageOff = true;
    }
    return config;
  }
  function makeTransportPortable(role: ConnectionRole) {
    store.config.transports[TARGET_KEY[role]] = portableTransportSelection(
      role,
      store.config.transports[TARGET_KEY[role]],
      store.transportDiscovery,
    );
  }
  /** Every writer of selection or settings calls this; it hands each server its new intent. */
  function selectIntent(): void {
    const key = JSON.stringify([
      store.selectedServers,
      store.latencySelection,
      store.config.transports.latencyTarget,
    ]);
    if (key !== idleEvidenceKey) {
      idleEvidenceKey = key;
      store.idleLatency = [];
    }
    for (const [id, connection] of connections)
      connection.select(
        store.serverCatalog && store.selectedServers.includes(id)
          ? serverConfig(id)
          : null,
      );
    host.limiter.drain();
  }
  function connect(server: ServerEntry, credentials: ServerCredentials) {
    connections.get(server.id)?.close();
    connections.set(
      server.id,
      new ServerConnection(server, { ...credentials, server }, host),
    );
  }

  function setMetadata(enabled: boolean) {
    metadataWanted = enabled;
    for (const connection of connections.values())
      if (!connection.config) connection.select(null);
    wake();
  }
  async function loadCatalog() {
    setMetadata(false);
    cancelServerApproval();
    catalogCheck?.abort();
    const request = new AbortController();
    catalogCheck = request;
    const signal = AbortSignal.any([
      request.signal,
      lifetime.signal,
      AbortSignal.timeout(5000),
    ]);
    store.catalogLoading = true;
    try {
      const catalog = await fetchCatalog(signal);
      signal.throwIfAborted();
      store.servers.clear();
      store.serverCatalog = catalog;
      const selection = reconcileSelection(
        catalog,
        readStored(SAVED_SELECTION_KEY),
      );
      store.selectedServers = selection.ids;
      store.unresolvedServers = selection.unresolved;
      // A shared preference must resolve within each selected server.
      for (const role of CONNECTION_ROLES) {
        const value = store.config.transports[TARGET_KEY[role]];
        const servers = catalog.servers.filter(
          (server) =>
            selection.ids.includes(server.id) &&
            (role !== "latency" ||
              store.latencySelection.mode === "all" ||
              server.id === store.primaryLatencyServer),
        );
        const origin =
          value !== "auto" &&
          !value.startsWith("protocol:") &&
          !value.startsWith("transport:");
        if (
          servers.length > 1 ||
          (origin &&
            servers.some(
              (server) =>
                !allowsServerOrigin(
                  server,
                  value.replace(/::(?:wt|wtdg)$/, ""),
                ),
            ))
        )
          makeTransportPortable(role);
      }
      const previous = new Map(connections);
      for (const connection of connections.values()) connection.close();
      connections.clear();
      for (const server of catalog.servers) {
        const old = previous.get(server.id);
        connect(
          server,
          old?.server.url === server.url
            ? old.credentials
            : serverCredentials(server),
        );
      }
      selectIntent();
      if (selection.unresolved.length)
        store.startError =
          "Saved servers have changed. Review the selection before starting.";
    } finally {
      if (catalogCheck === request) {
        catalogCheck = null;
        store.catalogLoading = false;
        wake();
      }
    }
  }
  async function retryCatalog() {
    if (!booted || store.isRunning || store.preparing) return;
    try {
      await loadCatalog();
      await check({ force: true });
    } catch (cause) {
      if (!lifetime.signal.aborted)
        store.startError =
          cause instanceof Error ? cause.message : "Could not load servers";
    }
  }
  function applyServers(ids: string[]): boolean {
    if (store.isRunning || store.preparing || !store.serverCatalog)
      return false;
    validateSelection(store.serverCatalog, ids);
    const next = selectedInCatalogOrder(store.serverCatalog, ids).map(
      (server) => server.id,
    );
    if (JSON.stringify(next) !== JSON.stringify(store.selectedServers)) {
      makeTransportPortable("throughput");
      if (
        store.latencySelection.mode === "all" ||
        !next.includes(store.primaryLatencyServer)
      )
        makeTransportPortable("latency");
    }
    if (approval && !ids.includes(approval.id)) cancelServerApproval();
    cancelPendingStart();
    store.selectedServers = next;
    store.unresolvedServers = [];
    selectIntent();
    writeStored(
      SAVED_SELECTION_KEY,
      selected().map(({ server: { id, url } }) => ({ id, url })),
    );
    return true;
  }
  function cancelServerApproval() {
    approval?.abort.abort();
    approval = null;
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
    const task = { id, abort: new AbortController() };
    approval = task;
    const { signal } = task.abort;
    const popup = openBrowserApprovalPopup(signal);
    try {
      const flow = await browserApproval(server);
      signal.throwIfAborted();
      store.serverApproval = { id, url: flow.url, code: flow.code };
      popup.navigate(flow.url);
      const context = await flow.poll(signal);
      const old = connections.get(id);
      if (approval !== task || old?.server.url !== context.server.url) return;
      connect(old.server, context);
      selectIntent();
      store.serverApproval = null;
      // Approval owns the grant exchange; the connection owns later path errors.
      void connections.get(id)?.check();
    } catch (cause) {
      if (signal.aborted) return;
      connections
        .get(id)
        ?.requireSignIn(
          cause instanceof Error ? cause.message : "Approval failed",
        );
      const limit = cause instanceof BrowserApprovalLimitError;
      if (store.serverApproval)
        store.serverApproval = {
          ...store.serverApproval,
          message: limit
            ? "Renew the remote login, then choose Sign in again."
            : "Approval did not finish. Try Sign in again.",
          renewUrl: limit ? `${server.url}/login` : undefined,
        };
    } finally {
      popup.close();
      if (approval === task) approval = null;
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
    selectIntent();
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
  }

  function ingest(event: RunnerEvent) {
    if (event.type === "serverFailure") {
      const { serverId, reason, message, scope } = event.failure;
      if (reason === "sign-in-required")
        connections.get(serverId)?.requireSignIn(message);
      else connections.get(serverId)?.invalidate([scope]);
    }
    if (event.type === "error")
      for (const connection of selected())
        connection.invalidate(CONNECTION_ROLES);
    store.ingest(event);
    if (
      event.type === "complete" ||
      event.type === "error" ||
      event.type === "phase"
    )
      wake();
  }
  function offline(serverId?: string | Event) {
    if (typeof serverId === "string")
      connections.get(serverId)?.invalidate(["latency"]);
    else
      for (const connection of selected())
        connection.invalidate(CONNECTION_ROLES);
  }
  function onlineAgain(serverId?: string | Event) {
    for (const connection of connections.values())
      if (typeof serverId !== "string" || connection.server.id === serverId)
        connection.resume();
  }
  function visibilityChanged() {
    if (hidden()) return wake();
    selectIntent();
    onlineAgain();
  }
  function cancelPendingStart() {
    pendingStart?.abort();
    pendingStart = null;
    store.startError = "";
    store.preparationStatus = "idle";
    wake();
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
    const { signal } = lifetime;
    window.addEventListener(
      AUTHENTICATION_REQUIRED_EVENT,
      onAuthenticationRequired,
    );
    await loadCatalog().catch((cause) => {
      if (signal.aborted) return;
      store.startError =
        cause instanceof Error ? cause.message : "Could not load servers";
    });
    if (signal.aborted) return;
    window.addEventListener("online", onlineAgain);
    window.addEventListener("offline", offline);
    document.addEventListener("visibilitychange", visibilityChanged);
    if (!hidden()) await check();
  }

  function toggleRun() {
    if (!booted) return;
    if (store.isRunning) {
      cancelPendingStart();
      runner?.abort();
      return;
    }
    if (pendingStart) return cancelPendingStart();
    const blocked = store.catalogLoading
      ? "Servers are still loading. Try again in a moment."
      : approval
        ? "Finish signing in to the selected server before starting."
        : store.startBlocker;
    if (blocked) {
      store.startError = blocked;
      store.preparationStatus = "blocked";
      return;
    }
    const config = $state.snapshot(store.config);
    const task = new AbortController();
    pendingStart = task;
    store.startError = "";
    store.preparationStatus = "authenticating";
    const live = () => pendingStart === task;
    void start(config, task.signal, live)
      .catch((cause) => {
        if (!live()) return;
        if (cause instanceof DOMException && cause.name === "AbortError")
          return;
        store.startError =
          cause instanceof Error ? cause.message : "Connection check failed";
        store.preparationStatus =
          store.preparationStatus === "authenticating" ? "blocked" : "failed";
      })
      .finally(() => {
        if (live()) {
          pendingStart = null;
          if (store.preparing) store.preparationStatus = "idle";
        }
        wake();
      });
  }
  async function start(
    config: RunnerConfig,
    signal: AbortSignal,
    live: () => boolean,
  ) {
    // The RTT that adapts warmup is known after the checks, so coverage assumes the longest.
    const plannedMs =
      buildSegments(adaptWarmup(config, Infinity)).totalMs +
      SESSION_RUN_MARGIN_MS;
    let budget = await requireSessionCoverage(plannedMs, signal);
    if (!live()) return;
    const servers = selected();
    for (const connection of servers) {
      const { credentials, server } = connection;
      if (credentials.kind !== "grant") continue;
      const remainingMs = (credentials.expiresAt ?? 0) - Date.now();
      if (remainingMs < plannedMs) {
        connection.requireSignIn(
          "Sign in again to cover the planned test duration",
        );
        throw new SessionCoverageError(
          `${server.name}: sign in again to cover the planned test duration`,
        );
      }
      if (!budget || remainingMs < budget.remainingMs)
        budget = {
          remainingMs,
          maximumLifetimeMs: remainingMs,
          checkedAt: performance.now(),
        };
    }
    sessionBudget = budget;
    store.preparationStatus = "checking";
    await Promise.all(
      servers.map((connection) => connection.check({ fresh: true, signal })),
    );
    if (!live()) return;
    const prepared: PreparedServer[] = [];
    const failures: string[] = [];
    for (const connection of servers) {
      const { server, view } = connection;
      const paths = connection.paths();
      if (paths) prepared.push({ server, paths });
      // A check superseded by newer intent ends the start without an error.
      else if (!view.message)
        throw new DOMException("Connection selection changed", "AbortError");
      else
        failures.push(
          servers.length > 1 ? `${server.name}: ${view.message}` : view.message,
        );
    }
    if (failures.length) throw new Error(failures.join("; "));
    const focus =
      (store.latencySelection.mode === "primary" &&
        prepared.find(
          (server) => server.server.id === store.primaryLatencyServer,
        )) ||
      prepared.reduce((best, next) =>
        (next.paths.latency?.rttMs ?? Infinity) <
        (best.paths.latency?.rttMs ?? Infinity)
          ? next
          : best,
      );
    store.reset();
    store.preparationStatus = "launching";
    store.latencyFocus = focus.server.id;
    releaseRunner();
    wake();
    const owner = createRunner(prepared, focus.server.id);
    runner = owner;
    // Only the current run may write the store, even through a retained callback.
    unsubscribe = owner.on((event) => {
      if (runner === owner) ingest(event);
    });
    runRttMs = focus.paths.latency?.rttMs ?? 0;
    const plan = adaptWarmup(config, runRttMs);
    store.run = { config: structuredClone(plan), servers: prepared };
    store.serverDetails = owner.details();
    owner.start(plan);
  }
  /** A superseded run can never deliver another event. */
  function releaseRunner() {
    unsubscribe?.();
    unsubscribe = undefined;
    runner?.dispose();
    runner = null;
  }
  function returnToStart() {
    cancelPendingStart();
    releaseRunner();
    store.reset();
    selectIntent();
  }
  /** The one writer of test settings; a running test accepts only its live settings. */
  function configureRun(patch: Partial<RunnerConfig>): boolean {
    if (store.preparing) return false;
    const liveKeys = ["stages", "duration", "adaptive", "visualization"];
    if (
      store.isRunning &&
      Object.keys(patch).some((k) => !liveKeys.includes(k))
    )
      return false;
    const config = { ...$state.snapshot(store.config), ...patch };
    if (
      !Object.values(config.stages).some(Boolean) ||
      Object.values(config.duration).some(
        (value) => !Number.isFinite(value) || value < 0,
      ) ||
      (Object.keys(config.stages) as StageKey[]).some(
        (stage) =>
          config.stages[stage] !== store.config.stages[stage] &&
          !store.canToggleStage(stage),
      )
    )
      return false;
    const plan = adaptWarmup(config, runRttMs);
    const live: LiveRunConfig = {
      stages: plan.stages,
      duration: plan.duration,
      adaptive: plan.adaptive,
    };
    const candidateTotal = buildSegments(plan).totalMs;
    if (store.isRunning) {
      const activeTotal = store.run
        ? buildSegments(store.run.config).totalMs
        : 0;
      const unsupported = store.run?.servers.find(({ paths }) =>
        uploadCapabilityFailure(config, paths.discovery),
      );
      try {
        if (
          !liveScheduleFitsSession(
            sessionBudget,
            activeTotal,
            candidateTotal,
            SESSION_RUN_MARGIN_MS,
          )
        )
          throw new Error(
            "This change would extend the test beyond the current session.",
          );
        if (unsupported)
          throw new Error(
            `${unsupported.server.name}: ${uploadCapabilityFailure(config, unsupported.paths.discovery)}`,
          );
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
      selectIntent();
      return true;
    }
    if (store.run)
      store.run = { ...store.run, config: { ...store.run.config, ...live } };
    return true;
  }
  function dispose() {
    booted = false;
    metadataWanted = false;
    lifetime.abort();
    catalogCheck?.abort();
    cancelServerApproval();
    for (const connection of connections.values()) connection.close();
    connections.clear();
    store.servers.clear();
    window.removeEventListener(
      AUTHENTICATION_REQUIRED_EVENT,
      onAuthenticationRequired,
    );
    cancelPendingStart();
    releaseRunner();
    window.removeEventListener("online", onlineAgain);
    window.removeEventListener("offline", offline);
    document.removeEventListener("visibilitychange", visibilityChanged);
    store.reset();
  }
  return {
    boot,
    dispose,
    retryCatalog,
    loadServerMetadata() {
      if (booted && store.serverCatalog && !store.isRunning && !store.preparing)
        setMetadata(true);
    },
    cancelServerMetadata: () => setMetadata(false),
    applyServers,
    signInServer,
    cancelServerApproval,
    focusServer,
    configureLatency,
    async retry({ id, role }: { id?: string; role?: ConnectionRole } = {}) {
      if (!booted || store.isRunning) return;
      cancelPendingStart();
      await Promise.all(
        selected()
          .filter((connection) => !id || connection.server.id === id)
          .map((connection) => connection.check({ force: true, role })),
      );
    },
    toggleRun,
    cancelPendingStart,
    hasPendingStart: () => pendingStart !== null,
    returnToStart,
    configureRun,
    toggleStage(stage: StageKey): boolean {
      if (!store.canToggleStage(stage)) return false;
      const stages = { ...$state.snapshot(store.config.stages) };
      if (stages[stage] && Object.values(stages).filter(Boolean).length <= 1)
        return false;
      stages[stage] = !stages[stage];
      return configureRun({ stages });
    },
    selectConnection(role: ConnectionRole, value: string) {
      cancelPendingStart();
      store.config.transports[TARGET_KEY[role]] = value;
      selectIntent();
    },
    restoreDefaults() {
      if (store.isRunning) return;
      cancelPendingStart();
      store.restoreTestDisplayDefaults();
      selectIntent();
    },
  };
}
export type ApplicationController = ReturnType<
  typeof createApplicationController
>;
