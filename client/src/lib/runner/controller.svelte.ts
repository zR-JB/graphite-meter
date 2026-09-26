import { untrack } from "svelte";
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
  ServerAuthenticationRequired,
  type ServerCredentials,
} from "../servers/credentials";
import { originLimiter } from "../servers/originLimiter";
import type {
  ConnectionRole,
  EngineInfo,
  LiveRunConfig,
  NetworkRunner,
  PreparedPaths,
  RunnerConfig,
  RunnerEvent,
  TransportDiscovery,
} from "./contract";
import { abortable, findCause, withinBudget } from "./abortable";
import { engineInfo, Run, type PreparedServer } from "./run";
import {
  BrowserOriginBlockedError,
  discoverServer,
  PreflightUnavailableError,
  prepareConnections,
  type ConnectionPreparation,
} from "./real/prepare";
import type {
  store as applicationStore,
  StageKey,
} from "../state/store.svelte";
import { canonicalAdaptiveConfig } from "../state/defaults";
import { readStored, writeStored } from "../state/persistence";
import { buildSegments } from "./schedule";
import {
  CONNECTION_FAILURE_REASONS,
  CONNECTION_FRESH_MS,
  CONNECTION_ROLES,
  connectionDraftKey,
  connectionDraftRoleKey,
  connectionFailureBackoff,
  connectionSelection,
  emptyConnectionValidation,
  latencyPathNeeded,
  preparedPaths,
  roleNeedsValidation,
  uploadCapabilityFailure,
  type ConnectionValidation,
  type ServerView,
  portableTransportSelection,
} from "./paths";

interface ApplicationDependencies {
  loadCatalog: (signal: AbortSignal) => Promise<ServerCatalog>;
  discover: typeof discoverServer;
  prepare: typeof prepareConnections;
  createRunner: (servers: PreparedServer[], focus: string) => NetworkRunner;
  describe: () => EngineInfo;
}
type Retry = { attempts: number; at: number; authentication: boolean };
/** At most one cancellable job per slot, with its own failure backoff. */
interface Slot<T> {
  retry: Retry;
  task?: { abort: AbortController; signal: AbortSignal; promise: Promise<T> };
}
interface RoleState extends Slot<void> {
  key: string;
  idle?: NonNullable<ConnectionPreparation["idle"]>;
}
interface ServerState {
  server: ServerEntry;
  credentials: ServerCredentials;
  config: RunnerConfig | null;
  discovery?: TransportDiscovery;
  preflight: Slot<TransportDiscovery | null>;
  discoveryError?: unknown;
  validation: ConnectionValidation;
  roles: Record<ConnectionRole, RoleState>;
}
interface CheckOptions {
  ids?: string[];
  role?: ConnectionRole;
  force?: boolean;
  /** Background retries check only roles whose backoff has elapsed. */
  due?: boolean;
  signal?: AbortSignal;
}

const SESSION_RUN_MARGIN_MS = 60_000;
const SAVED_SELECTION_KEY = "graphite-meter:server-selection:v1";
const retryState = (): Retry => ({ attempts: 0, at: 0, authentication: false });
const aborted = () =>
  new DOMException("Connection selection changed", "AbortError");

function connectionFailureMessage(
  cause: unknown,
  server?: ServerEntry,
): string {
  const authentication = findCause(cause, ServerAuthenticationRequired);
  if (authentication) return authentication.message;
  if (cause instanceof BrowserOriginBlockedError) return cause.message;
  if (cause instanceof PreflightUnavailableError) {
    const seen = new Set<unknown>();
    let error: unknown = cause.cause;
    while (error && typeof error === "object" && !seen.has(error)) {
      const detail = error as {
        name?: string;
        message?: string;
        cause?: unknown;
      };
      if (
        detail.name === "NetworkError" ||
        /failed to fetch|fetch failed|network(?:error| request failed)|load failed|connection (?:refused|reset|lost)/i.test(
          detail.message ?? "",
        )
      )
        return server?.url.startsWith("https://") &&
          location.protocol === "http:"
          ? "Server could not be reached. If it requires sign-in, open this interface over HTTPS."
          : "Server could not be reached";
      seen.add(error);
      error = detail.cause;
    }
  }
  return cause instanceof DOMException && cause.name === "TimeoutError"
    ? "Connection check timed out"
    : "Connection check failed";
}

const sameRole = (
  a: ConnectionValidation[ConnectionRole],
  b: ConnectionValidation[ConnectionRole],
) =>
  a.state === b.state &&
  a.path === b.path &&
  a.message === b.message &&
  a.selection === b.selection;
const sameView = (a: ServerView, b: ServerView) =>
  a.server === b.server &&
  a.discovery === b.discovery &&
  a.readiness === b.readiness &&
  a.message === b.message &&
  a.metadataChecking === b.metadataChecking &&
  CONNECTION_ROLES.every((role) =>
    sameRole(a.validation[role], b.validation[role]),
  );

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
  const prepare = dependencies.prepare ?? prepareConnections;
  const fetchCatalog = dependencies.loadCatalog ?? loadServerCatalog;
  const discover = dependencies.discover ?? discoverServer;
  const createRunner =
    dependencies.createRunner ??
    ((servers: PreparedServer[], focus: string) => new Run(servers, focus));
  const describe = dependencies.describe ?? engineInfo;

  const servers = new Map<string, ServerState>();
  const limiter = originLimiter();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let metadataWanted = false;
  let runner: NetworkRunner | null = null;
  let activeCapabilities: { name: string; uploadCheckpoint?: boolean }[] = [];
  let unsubscribe: (() => void) | undefined;
  let disposeDraft: (() => void) | undefined;
  let pendingStart: { abort: AbortController; draft: string } | null = null;
  let booted = false;
  let lifetime = new AbortController();
  let catalogCheck: AbortController | null = null;
  let sessionBudget: SessionBudget | null = null;
  let approval: AbortController | null = null;
  let approvalServerId: string | null = null;
  let idleEvidenceKey = "";

  const hidden = () => document.visibilityState === "hidden";
  const active = () =>
    booted &&
    !store.catalogLoading &&
    !store.isRunning &&
    !pendingStart &&
    !hidden();
  const selectionKey = () =>
    JSON.stringify([
      store.latencySelection.mode,
      store.primaryLatencyServer,
      store.selectedServers.map((id) => [
        id,
        store.serverCatalog?.servers.find((server) => server.id === id)?.url,
      ]),
    ]);
  const draftKey = (config: RunnerConfig) =>
    JSON.stringify([connectionDraftKey(config), selectionKey()]);
  const catalogSelected = () =>
    store.serverCatalog
      ? selectedInCatalogOrder(store.serverCatalog, store.selectedServers)
      : [];
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
    const key = role === "throughput" ? "throughputTarget" : "latencyTarget";
    store.config.transports[key] = portableTransportSelection(
      role,
      store.config.transports[key],
      store.transportDiscovery,
    );
  }

  const current = (state: ServerState) =>
    booted && servers.get(state.server.id) === state;
  const roleKeys = (state: ServerState) =>
    CONNECTION_ROLES.map((role) => state.roles[role].key).join("\n");
  const expiredGrant = (state: ServerState) =>
    state.credentials.kind === "grant" &&
    (state.credentials.expiresAt ?? 0) <= Date.now();
  function required({ config }: ServerState): ConnectionRole[] {
    if (!config) return [];
    return CONNECTION_ROLES.filter(
      (role) => role !== "latency" || latencyPathNeeded(config),
    );
  }
  const needsCheck = (state: ServerState, role: ConnectionRole) =>
    !!state.config &&
    roleNeedsValidation(state.config, state.validation, role, state.discovery);
  const setRole = (
    state: ServerState,
    role: ConnectionRole,
    check: Partial<ConnectionValidation[ConnectionRole]>,
  ) => {
    state.validation = {
      ...state.validation,
      [role]: { ...state.validation[role], ...check },
    };
  };
  function paths(
    state: ServerState | undefined,
    maxAgeMs = CONNECTION_FRESH_MS,
  ): PreparedPaths | null {
    if (!state?.config || expiredGrant(state) || state.discoveryError)
      return null;
    const prepared = preparedPaths(
      state.config,
      state.discovery ?? null,
      state.validation,
      maxAgeMs,
    );
    return prepared && { ...prepared, credentials: state.credentials };
  }
  const readySelected = (maxAgeMs = CONNECTION_FRESH_MS) =>
    !!store.serverCatalog &&
    !store.unresolvedServers.length &&
    store.selectedServers.length > 0 &&
    store.selectedServers.every((id) => paths(servers.get(id), maxAgeMs));

  function view(state: ServerState): ServerView {
    const expired = expiredGrant(state);
    const roles = required(state);
    const failed = roles.find(
      (role) => state.validation[role].state === "failed",
    );
    const message = expired
      ? new ServerAuthenticationRequired(state.server).message
      : state.discoveryError
        ? connectionFailureMessage(state.discoveryError, state.server)
        : (state.config &&
            uploadCapabilityFailure(state.config, state.discovery)) ||
          (failed && state.validation[failed].message) ||
          undefined;
    let validation = state.validation;
    const { config } = state;
    if (config && (state.preflight.task || state.discoveryError))
      for (const role of roles)
        validation = {
          ...validation,
          [role]: {
            selection: connectionSelection(config, role),
            state: state.preflight.task ? "checking" : "failed",
            path: null,
            ...(message ? { message } : {}),
          },
        };
    return {
      server: state.server,
      discovery: state.discovery ?? null,
      validation,
      metadataChecking: !state.config && !!state.preflight.task,
      readiness:
        expired ||
        (!!state.discoveryError && state.preflight.retry.authentication) ||
        roles.some((role) => state.roles[role].retry.authentication)
          ? "sign-in"
          : message
            ? "failed"
            : state.preflight.task ||
                roles.some((role) => state.roles[role].task)
              ? "checking"
              : paths(state)
                ? "ready"
                : "unchecked",
      ...(message ? { message } : {}),
    };
  }
  /** Replaces the server's view only when something visible changed. */
  function publish(state: ServerState): void {
    if (!current(state)) return;
    const next = view(state);
    const previous = store.servers.get(state.server.id);
    if (previous && sameView(previous, next)) return;
    store.servers.set(state.server.id, next);
    const entry = store.serverCatalog?.servers.find(
      (server) => server.id === state.server.id,
    );
    if (entry && entry.name !== next.server.name) entry.name = next.server.name;
    if (entry && entry.location !== next.server.location)
      entry.location = next.server.location;
    // A verified selection clears an offline verdict.
    if (next.readiness === "ready" && readySelected(Infinity))
      store.connectivity = "connected";
  }
  function failed(retry: Retry, error: unknown): void {
    retry.authentication = !!findCause(error, ServerAuthenticationRequired);
    retry.at = retry.authentication
      ? Infinity
      : Date.now() + connectionFailureBackoff(++retry.attempts);
  }
  function stopIdle(slot: RoleState): void {
    const idle = slot.idle;
    slot.idle = undefined;
    idle?.stop();
  }
  function cancelRole(
    state: ServerState,
    role: ConnectionRole,
    discardIdle = true,
  ): void {
    const slot = state.roles[role];
    const task = slot.task;
    slot.task = undefined;
    task?.abort.abort(aborted());
    if (discardIdle) stopIdle(slot);
    if (state.validation[role].state === "checking")
      setRole(state, role, { state: "stale", path: null });
  }
  function cancelDiscovery(state: ServerState): void {
    const task = state.preflight.task;
    state.preflight.task = undefined;
    task?.abort.abort(aborted());
  }
  function cancelServer(state: ServerState): void {
    cancelDiscovery(state);
    for (const role of CONNECTION_ROLES) cancelRole(state, role);
  }

  function resetServers(catalog: readonly ServerEntry[]): void {
    const previous = new Map(servers);
    clearTimeout(timer);
    for (const state of servers.values()) cancelServer(state);
    servers.clear();
    for (const server of catalog) {
      const old = previous.get(server.id);
      const state: ServerState = {
        server,
        credentials:
          old?.server.url === server.url
            ? { ...old.credentials, server }
            : serverCredentials(server),
        config: null,
        preflight: { retry: retryState() },
        validation: emptyConnectionValidation(),
        roles: {
          throughput: { key: "", retry: retryState() },
          latency: { key: "", retry: retryState() },
        },
      };
      servers.set(server.id, state);
      publish(state);
    }
  }

  /** Changed intent cancels only its role; unchanged peers keep verified paths. */
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
    const selected = store.serverCatalog ? store.selectedServers : [];
    for (const state of servers.values()) {
      if (!selected.includes(state.server.id)) {
        if (state.config) {
          for (const role of CONNECTION_ROLES) {
            cancelRole(state, role, false);
            state.roles[role].idle?.stop();
          }
          if (!metadataWanted) cancelDiscovery(state);
        }
        state.config = null;
        continue;
      }
      const config = serverConfig(state.server.id);
      state.config = config;
      // A restored selection must not promote expired evidence back to Ready.
      if (
        state.discovery &&
        Date.now() - state.discovery.fetchedAt > CONNECTION_FRESH_MS
      ) {
        for (const role of CONNECTION_ROLES) cancelRole(state, role, false);
        state.discovery = undefined;
      }
      for (const role of CONNECTION_ROLES) {
        const slot = state.roles[role];
        const roleKey = connectionDraftRoleKey(config, role);
        const path = state.validation[role].path;
        const expired =
          !!path && Date.now() - path.verifiedAt > CONNECTION_FRESH_MS;
        if (slot.key === roleKey && !expired) continue;
        slot.key = roleKey;
        const stale = expired || needsCheck(state, role);
        const unused = role === "latency" && !latencyPathNeeded(config);
        cancelRole(state, role, stale || unused);
        slot.retry = retryState();
        if (state.discoveryError) state.preflight.retry = retryState();
        const selection = connectionSelection(config, role);
        if (!unused && stale)
          state.validation = {
            ...state.validation,
            [role]: { selection, state: "stale", path: null },
          };
        else setRole(state, role, { selection });
      }
    }
    limiter.drain();
    for (const state of servers.values()) publish(state);
    schedule();
  }
  function invalidate(
    roles: ConnectionRole[],
    ids: string[] = store.selectedServers,
  ): void {
    for (const id of ids) {
      const state = servers.get(id);
      if (!state) continue;
      for (const role of roles) {
        if (state.validation[role].state !== "verified") continue;
        const failedAdoption =
          !!state.roles[role].task && !!state.roles[role].idle;
        cancelRole(state, role);
        state.roles[role].retry = retryState();
        if (failedAdoption)
          failed(state.roles[role].retry, new Error("Idle connection lost"));
        setRole(state, role, { state: "stale", path: null });
      }
      publish(state);
    }
    schedule();
  }
  function requireAuthentication(id: string, message: string): void {
    const state = servers.get(id);
    if (!state) return;
    cancelServer(state);
    const error = new ServerAuthenticationRequired(state.server);
    error.message = message;
    state.discoveryError = error;
    failed(state.preflight.retry, error);
    state.validation = emptyConnectionValidation();
    publish(state);
    schedule();
  }
  function authorize(credentials: ServerCredentials): void {
    const state = servers.get(credentials.server.id);
    if (!state || state.server.url !== credentials.server.url) return;
    cancelServer(state);
    state.credentials = { ...credentials, server: state.server };
    state.discovery = undefined;
    state.discoveryError = undefined;
    state.preflight.retry = retryState();
    state.validation = emptyConnectionValidation();
    for (const role of CONNECTION_ROLES) state.roles[role].retry = retryState();
    publish(state);
    schedule();
  }

  /** Runs one origin-limited job in a slot; a failure is recorded by `fail`, never rethrown. */
  function job<T>(
    state: ServerState,
    slot: Slot<T>,
    options: { owner?: AbortSignal; priority: () => number; timeoutMs: number },
    run: (signal: AbortSignal, live: () => boolean) => Promise<T>,
    fail: (error: unknown) => T,
  ): Promise<T> {
    const abort = new AbortController();
    const { owner } = options;
    const signal = owner
      ? AbortSignal.any([abort.signal, owner])
      : abort.signal;
    const live = () =>
      current(state) && slot.task?.abort === abort && !signal.aborted;
    const work = async () => {
      const origin = new URL(state.server.url).origin;
      const release = await limiter.acquire(origin, options.priority, signal);
      try {
        return await withinBudget(signal, options.timeoutMs, (inner) =>
          run(inner, live),
        );
      } finally {
        release();
      }
    };
    const promise = work()
      .catch((error) => fail(live() ? error : undefined))
      .finally(() => {
        if (slot.task?.abort !== abort) return;
        slot.task = undefined;
        publish(state);
        schedule();
      });
    slot.task = { abort, signal, promise };
    publish(state);
    return promise;
  }
  /** Starts or joins checks; outcomes land in each server's view and never reject. */
  function refresh(options: CheckOptions = {}) {
    if (!booted || !store.serverCatalog || store.unresolvedServers.length)
      return { states: [], done: Promise.resolve() };
    selectIntent();
    const states = (options.ids ?? store.selectedServers).flatMap((id) => {
      const state = servers.get(id);
      return state?.config ? [state] : [];
    });
    if (options.force)
      for (const state of states)
        for (const role of required(state))
          if (!options.role || role === options.role) cancelRole(state, role);
    const work = Promise.all(
      states.map((state) => checkServer(state, options)),
    );
    return { states, done: work.then(() => {}) };
  }
  /** A check whose own jobs were superseded rejects as aborted; otherwise any failure throws. */
  async function validate(options: CheckOptions = {}): Promise<void> {
    if (!booted) throw new DOMException("Runner is not active", "AbortError");
    if (!store.serverCatalog)
      throw new Error("The server catalog is unavailable");
    if (store.unresolvedServers.length || !store.selectedServers.length)
      throw new Error("Review the saved server selection");
    const expected = (options.ids ?? store.selectedServers).length;
    const { states, done } = refresh(options);
    if (states.length !== expected) throw aborted();
    const keys = states.map(roleKeys);
    await (options.signal ? abortable(done, options.signal) : done);
    const superseded = (state: ServerState, i: number) =>
      !current(state) ||
      !state.config ||
      keys[i] !== roleKeys(state) ||
      (!state.discoveryError &&
        (!state.discovery ||
          required(state).some((role) =>
            ["checking", "stale"].includes(state.validation[role].state),
          )));
    if (states.some(superseded)) throw aborted();
    const maxAgeMs = options.signal ? CONNECTION_FRESH_MS : Infinity;
    const failures = states.filter((state) => !paths(state, maxAgeMs));
    if (failures.length)
      throw new Error(
        failures
          .map((state) => {
            const message =
              view(state).message ??
              "Enabled measurements could not be prepared";
            return states.length > 1
              ? `${state.server.name}: ${message}`
              : message;
          })
          .join("; "),
      );
  }
  async function checkServer(
    state: ServerState,
    options: CheckOptions,
  ): Promise<void> {
    const keys = roleKeys(state);
    const { signal } = options;
    const discovery = await discoverOnce(
      state,
      !!signal,
      !!options.force,
      signal,
    );
    const config = state.config;
    if (!discovery || !config || signal?.aborted || !current(state)) return;
    if (keys !== roleKeys(state)) return;
    const now = Date.now();
    const roles = required(state).filter((role) => {
      const { state: status, path } = state.validation[role];
      if (options.force && (!options.role || role === options.role))
        return true;
      if (options.role && role !== options.role && status === "failed")
        return false;
      if (options.due && state.roles[role].retry.at > now) return false;
      return (
        roleNeedsValidation(config, state.validation, role, discovery) ||
        (!!signal && !!path && now - path.verifiedAt > CONNECTION_FRESH_MS)
      );
    });
    await Promise.all(
      roles.map((role) => probe(state, role, config, discovery, signal)),
    );
  }
  /** Resolves to null when discovery failed or was superseded; the view carries why. */
  async function discoverOnce(
    state: ServerState,
    fresh: boolean,
    force = false,
    owner?: AbortSignal,
  ): Promise<TransportDiscovery | null> {
    if (state.preflight.task && !state.preflight.task.signal.aborted)
      return state.preflight.task.promise;
    if (
      state.discovery &&
      !state.discoveryError &&
      !force &&
      (!fresh || Date.now() - state.discovery.fetchedAt <= CONNECTION_FRESH_MS)
    )
      return state.discovery;
    state.discoveryError = undefined;
    return job(
      state,
      state.preflight,
      { owner, priority: () => (state.config ? 0 : 1), timeoutMs: 5000 },
      async (signal, live) => {
        const discovery = await discover(signal, state.credentials);
        signal.throwIfAborted();
        if (!live()) throw aborted();
        const changedGeneration =
          !!state.discovery &&
          state.discovery.generation !== discovery.generation;
        state.discovery = discovery;
        state.discoveryError = undefined;
        state.preflight.retry = retryState();
        state.server = {
          ...state.server,
          name: discovery.server.name || state.server.name,
          location: discovery.server.location,
        };
        state.credentials = { ...state.credentials, server: state.server };
        for (const role of CONNECTION_ROLES) {
          const path = state.validation[role].path;
          if (
            changedGeneration ||
            (path && path.generation !== discovery.generation)
          ) {
            cancelRole(state, role);
            setRole(state, role, { state: "stale", path: null });
            state.roles[role].retry = retryState();
          }
        }
        return discovery;
      },
      (error) => {
        if (error !== undefined) {
          state.discoveryError = error;
          failed(state.preflight.retry, error);
        }
        return null;
      },
    );
  }
  function probe(
    state: ServerState,
    role: ConnectionRole,
    config: RunnerConfig,
    discovery: TransportDiscovery,
    owner?: AbortSignal,
  ): Promise<void> {
    const slot = state.roles[role];
    if (slot.task && !slot.task.signal.aborted) return slot.task.promise;
    const key = slot.key;
    stopIdle(slot);
    state.validation = {
      ...state.validation,
      [role]: {
        selection: connectionSelection(config, role),
        state: "checking",
        path: null,
      },
    };
    const matches = () =>
      !!state.config &&
      slot.key === key &&
      state.discovery?.generation === discovery.generation;
    return job(
      state,
      slot,
      { owner, priority: () => 0, timeoutMs: 12000 },
      async (signal, live) => {
        const result = await prepare(
          config,
          state.validation,
          [role],
          signal,
          state.credentials,
          discovery,
        );
        if (signal.aborted || !live() || !matches()) {
          result.idle?.stop();
          throw signal.reason ?? aborted();
        }
        // The caller owns exactly one role, whatever else the adapter returns.
        const checked = result.validation[role];
        state.validation = { ...state.validation, [role]: checked };
        if (result.failure || checked.state !== "verified" || !checked.path) {
          result.idle?.stop();
          throw (
            result.failure ??
            new Error(checked.message ?? "Connection check failed")
          );
        }
        slot.retry = retryState();
        // Only this server's idle latency is shown before a run.
        if (role === "latency" && result.idle && state.server.id === "self")
          adoptIdle(state, slot, result.idle);
        else result.idle?.stop();
      },
      (error) => {
        if (error === undefined || !matches()) return;
        failed(slot.retry, error);
        state.validation = {
          ...state.validation,
          [role]: {
            selection: connectionSelection(config, role),
            state: "failed",
            path: null,
            message: connectionFailureMessage(error, state.server),
          },
        };
      },
    );
  }
  function adoptIdle(
    state: ServerState,
    slot: RoleState,
    monitor: NonNullable<RoleState["idle"]>,
  ): void {
    slot.idle = monitor;
    const id = state.server.id;
    monitor.onEvent = (event) => {
      if (
        slot.idle !== monitor ||
        !state.config ||
        idleServer() !== id ||
        !current(state)
      )
        return;
      if (event.type === "latency")
        return store.ingest({
          type: "serverLatency",
          serverId: id,
          sample: event.sample,
        });
      store.connectivity = event.state;
      if (event.state === "offline") offline(id);
      else onlineAgain(id);
    };
  }

  const idleServer = () =>
    active() &&
    store.representativeServerId === "self" &&
    latencyPathNeeded(store.config)
      ? "self"
      : null;
  function refreshIdle(): void {
    const owner = idleServer();
    for (const state of servers.values()) {
      const idle = state.roles.latency.idle;
      if (!idle) continue;
      if (
        state.server.id === owner &&
        state.config &&
        !expiredGrant(state) &&
        !state.discoveryError &&
        !needsCheck(state, "latency")
      )
        idle.start();
      else idle.stop();
    }
  }
  function dueAt(state: ServerState): number {
    let at =
      state.credentials.kind === "grant" &&
      !state.preflight.retry.authentication
        ? (state.credentials.expiresAt ?? 0)
        : Infinity;
    if (state.preflight.task) return at;
    if (!state.config) {
      if (
        metadataWanted &&
        (!state.discovery ||
          Date.now() - state.discovery.fetchedAt > CONNECTION_FRESH_MS)
      )
        at = Math.min(at, state.preflight.retry.at);
    } else if (!state.discovery || state.discoveryError)
      at = Math.min(at, state.preflight.retry.at);
    else
      for (const role of required(state))
        if (!state.roles[role].task && needsCheck(state, role))
          at = Math.min(at, state.roles[role].retry.at);
    return at;
  }
  function schedule(): void {
    refreshIdle();
    clearTimeout(timer);
    timer = undefined;
    if (!active()) return;
    const at = Math.min(...[...servers.values()].map(dueAt));
    if (Number.isFinite(at))
      timer = setTimeout(pump, Math.max(0, at - Date.now()));
  }
  function pump(): void {
    timer = undefined;
    if (!active()) return;
    const now = Date.now();
    for (const state of servers.values()) {
      if (dueAt(state) > now) continue;
      if (expiredGrant(state) && !state.preflight.retry.authentication)
        requireAuthentication(
          state.server.id,
          new ServerAuthenticationRequired(state.server).message,
        );
      else if (!state.config) void discoverOnce(state, true);
      else void checkServer(state, { due: true });
    }
    schedule();
  }

  function setMetadata(enabled: boolean) {
    metadataWanted = enabled;
    if (!enabled)
      for (const state of servers.values())
        if (!state.config) {
          cancelDiscovery(state);
          publish(state);
        }
    schedule();
  }
  async function loadCatalog() {
    setMetadata(false);
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
      resetServers(catalog.servers);
      selectIntent();
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
  async function retryCatalog() {
    if (!booted || store.isRunning || store.preparing) return;
    try {
      await loadCatalog();
      await validate({ force: true });
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
    if (approvalServerId && !ids.includes(approvalServerId))
      cancelServerApproval();
    cancelPendingStart();
    store.selectedServers = next;
    store.unresolvedServers = [];
    selectIntent();
    writeStored(
      SAVED_SELECTION_KEY,
      catalogSelected().map(({ id, url }) => ({ id, url })),
    );
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
    const task = new AbortController();
    approval = task;
    approvalServerId = id;
    const popup = openBrowserApprovalPopup(task.signal);
    try {
      const flow = await browserApproval(server);
      task.signal.throwIfAborted();
      store.serverApproval = { id, url: flow.url, code: flow.code };
      popup.navigate(flow.url);
      const context = await flow.poll(task.signal);
      if (approval !== task) return;
      authorize(context);
      store.serverApproval = null;
      // Approval owns the grant exchange; validation owns later path errors.
      void refresh().done;
    } catch (cause) {
      if (!task.signal.aborted) {
        requireAuthentication(
          id,
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
      if (event.failure.reason === "sign-in-required")
        requireAuthentication(event.failure.serverId, event.failure.message);
      else invalidate([event.failure.scope], [event.failure.serverId]);
    }
    if (
      event.type === "error" &&
      CONNECTION_FAILURE_REASONS.has(event.error.reason)
    )
      invalidate(CONNECTION_ROLES);
    store.ingest(event);
    if (
      event.type === "complete" ||
      event.type === "error" ||
      event.type === "phase"
    )
      schedule();
  }
  function offline(serverId?: string | Event) {
    if (typeof serverId === "string") invalidate(["latency"], [serverId]);
    else invalidate(CONNECTION_ROLES);
    selectIntent();
  }
  function onlineAgain(serverId?: string | Event) {
    for (const state of servers.values()) {
      if (typeof serverId === "string" && state.server.id !== serverId)
        continue;
      if (!state.preflight.retry.authentication) state.preflight.retry.at = 0;
      for (const role of CONNECTION_ROLES)
        if (!state.roles[role].retry.authentication)
          state.roles[role].retry.at = 0;
    }
    selectIntent();
  }
  function visibilityChanged() {
    if (hidden()) schedule();
    else selectIntent();
  }
  function cancelPendingStart() {
    pendingStart?.abort.abort();
    pendingStart = null;
    store.startError = "";
    store.preparationStatus = "idle";
    schedule();
  }
  async function validateConnections(
    force = false,
    role?: ConnectionRole,
    signal?: AbortSignal,
  ): Promise<void> {
    if (force && pendingStart) cancelPendingStart();
    return validate({ force, role, signal });
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
    let draft = [
      selectionKey(),
      ...CONNECTION_ROLES.map((role) =>
        connectionDraftRoleKey(store.config, role),
      ),
    ].join("\n");
    disposeDraft = $effect.root(() => {
      $effect(() => {
        const next = [
          selectionKey(),
          ...CONNECTION_ROLES.map((role) =>
            connectionDraftRoleKey(store.config, role),
          ),
        ].join("\n");
        if (next === draft) return;
        draft = next;
        untrack(() => {
          if (pendingStart && pendingStart.draft !== draftKey(store.config))
            cancelPendingStart();
          selectIntent();
        });
      });
    });
    window.addEventListener("online", onlineAgain);
    window.addEventListener("offline", offline);
    document.addEventListener("visibilitychange", visibilityChanged);
    if (hidden()) selectIntent();
    else await refresh().done;
  }

  function blockStart(message: string) {
    store.startError = message;
    store.preparationStatus = "blocked";
  }
  function toggleRun() {
    if (!booted) return;
    if (store.isRunning) {
      cancelPendingStart();
      runner?.abort();
      return;
    }
    if (pendingStart) return cancelPendingStart();
    if (store.catalogLoading || !store.serverCatalog)
      return blockStart(
        store.catalogLoading
          ? "Servers are still loading. Try again in a moment."
          : "Open Settings to retry loading the server list.",
      );
    if (approval)
      return blockStart(
        "Finish signing in to the selected server before starting.",
      );
    if (store.unresolvedServers.length || !store.selectedServers.length)
      return blockStart(
        "The saved selection changed. Open Settings to choose the servers to test.",
      );
    const config = $state.snapshot(store.config);
    config.adaptive = canonicalAdaptiveConfig(config.adaptive);
    const abort = new AbortController();
    const task = { abort, draft: draftKey(config) };
    pendingStart = task;
    store.startError = "";
    store.preparationStatus = "authenticating";
    const plannedMs = buildSegments(config).totalMs + SESSION_RUN_MARGIN_MS;
    const live = () =>
      booted &&
      pendingStart === task &&
      !abort.signal.aborted &&
      draftKey(store.config) === task.draft;
    const start = async () => {
      const budget = await requireSessionCoverage(plannedMs, abort.signal);
      if (!live()) return;
      sessionBudget = budget;
      for (const server of catalogSelected()) {
        const context = servers.get(server.id)?.credentials;
        if (context?.kind !== "grant") continue;
        const remainingMs = (context.expiresAt ?? 0) - Date.now();
        if (remainingMs < plannedMs) {
          requireAuthentication(
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
      await validate({ signal: abort.signal });
      if (!live()) return;
      const prepared = catalogSelected().flatMap((server) => {
        const verified = paths(servers.get(server.id));
        return verified ? [{ server, paths: verified }] : [];
      });
      if (prepared.length !== store.selectedServers.length) throw aborted();
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
      store.preparationStatus = "launching";
      store.latencyFocus = focus.server.id;
      releaseRunner();
      schedule();
      activeCapabilities = prepared.map(({ server, paths }) => ({
        name: server.name,
        uploadCheckpoint: paths.discovery.uploadCheckpoint,
      }));
      const owner = createRunner(prepared, focus.server.id);
      runner = owner;
      // Only the current run may write the store, even through a retained callback.
      unsubscribe = owner.on((event) => {
        if (runner === owner) ingest(event);
      });
      store.activeConfig = structuredClone(config);
      store.activeServers = prepared.map(({ server, paths }) => ({
        server: { ...server },
        paths,
      }));
      store.serverDetails = owner.details();
      owner.start(config, focus.paths.latency?.rttMs ?? 0);
    };
    void start()
      .catch((cause) => {
        if (!live()) return;
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
        if (pendingStart === task) {
          pendingStart = null;
          if (store.preparing) store.preparationStatus = "idle";
        }
        schedule();
      });
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
    const candidateTotal = buildSegments(config).totalMs;
    if (store.isRunning) {
      const activeTotal = store.activeConfig
        ? buildSegments(store.activeConfig).totalMs
        : 0;
      if (
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
      selectIntent();
      return true;
    }
    store.compactThroughputForDuration(candidateTotal);
    if (store.activeConfig)
      store.activeConfig = { ...store.activeConfig, ...live };
    return true;
  }
  function dispose() {
    booted = false;
    metadataWanted = false;
    lifetime.abort();
    catalogCheck?.abort();
    cancelServerApproval();
    clearTimeout(timer);
    for (const state of servers.values()) cancelServer(state);
    servers.clear();
    store.servers.clear();
    window.removeEventListener(
      AUTHENTICATION_REQUIRED_EVENT,
      onAuthenticationRequired,
    );
    disposeDraft?.();
    cancelPendingStart();
    releaseRunner();
    activeCapabilities = [];
    window.removeEventListener("online", onlineAgain);
    window.removeEventListener("offline", offline);
    document.removeEventListener("visibilitychange", visibilityChanged);
    store.reset();
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
    selectIntent();
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
    /** An explicit retry refreshes only this participant, including capabilities. */
    retryServer(id: string): Promise<void> {
      if (
        !store.selectedServers.includes(id) ||
        store.isRunning ||
        store.preparing
      )
        return Promise.resolve();
      return refresh({ ids: [id], force: true }).done;
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
