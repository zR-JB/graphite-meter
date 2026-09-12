import { abortable, withinBudget } from "../runner/abortable";
import type {
  ConnectionRole,
  PreparedPaths,
  RunnerConfig,
  RunnerEvent,
  TransportDiscovery,
} from "../runner/contract";
import {
  CONNECTION_FRESH_MS,
  CONNECTION_ROLES,
  connectionDraftRoleKey,
  connectionFailureBackoff,
  connectionSelection,
  emptyConnectionValidation,
  latencyPathNeeded,
  preparedPaths,
  roleNeedsValidation,
  uploadCapabilityFailure,
  type ConnectionValidation,
} from "../runner/connectionModel";
import {
  type ConnectionPreparation,
  discoverServer,
  prepareConnections,
} from "../runner/real/prepare";
import {
  BrowserOriginBlockedError,
  PreflightUnavailableError,
} from "../runner/real/transportError";
import {
  serverCredentials,
  ServerAuthenticationRequired,
  type ServerCredentials,
} from "./credentials";
import type { ServerEntry } from "./catalog";

type Monitor = NonNullable<ConnectionPreparation["idle"]>;
type Operation<T> = { abort: AbortController; promise: Promise<T> };
type Retry = { attempts: number; at: number; authentication: boolean };
interface RoleState {
  key: string;
  retry: Retry;
  task?: Operation<void>;
  idle?: Monitor;
}
interface ServerState {
  server: ServerEntry;
  credentials: ServerCredentials;
  config: RunnerConfig | null;
  discovery?: TransportDiscovery;
  discoveryTask?: Operation<TransportDiscovery>;
  discoveryRetry: Retry;
  discoveryError?: unknown;
  validation: ConnectionValidation;
  roles: Record<ConnectionRole, RoleState>;
}
export interface ServerConnectionView {
  server: ServerEntry;
  discovery?: TransportDiscovery;
  validation: ConnectionValidation;
  readiness: {
    state: "unchecked" | "checking" | "ready" | "sign-in" | "failed";
    message?: string;
    checkedAt?: number;
  };
  metadataChecking: boolean;
}
interface Dependencies {
  discover: typeof discoverServer;
  prepare: typeof prepareConnections;
  changed: (view: ServerConnectionView) => void;
  idleEvent: (id: string, event: RunnerEvent) => void;
}
const retryState = (): Retry => ({ attempts: 0, at: 0, authentication: false });
const roleState = (): RoleState => ({ key: "", retry: retryState() });
const aborted = () =>
  new DOMException("Connection selection changed", "AbortError");

function authenticationFailure(cause: unknown): boolean {
  const seen = new Set<unknown>();
  while (cause instanceof Error && !seen.has(cause)) {
    if (cause instanceof ServerAuthenticationRequired) return true;
    seen.add(cause);
    cause = cause.cause;
  }
  return false;
}
export function connectionFailureMessage(cause: unknown): string {
  if (
    cause instanceof BrowserOriginBlockedError ||
    cause instanceof ServerAuthenticationRequired
  )
    return cause.message;
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
        return "Server could not be reached";
      seen.add(error);
      error = detail.cause;
    }
  }
  return cause instanceof DOMException && cause.name === "TimeoutError"
    ? "Connection check timed out"
    : "Connection check failed";
}

/** One owner for discovery, intent, checks, retries and idle resources, for one or many selected servers. */
export class ServerConnections {
  readonly #deps: Dependencies;
  readonly #servers = new Map<string, ServerState>();
  #selected: string[] = [];
  #enabled = false;
  #metadata = false;
  #idleServer: string | null = null;
  #disposed = false;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #active = 0;
  #queue: { priority: () => number; start: () => void }[] = [];

  constructor(dependencies: Dependencies) {
    this.#deps = dependencies;
  }

  reset(servers: readonly ServerEntry[]): void {
    this.#disposed = false;
    const previous = new Map(this.#servers);
    this.#stop();
    this.#servers.clear();
    this.#selected = [];
    for (const server of servers) {
      const old = previous.get(server.id);
      const state: ServerState = {
        server,
        credentials:
          old?.server.url === server.url
            ? { ...old.credentials, server }
            : serverCredentials(server),
        config: null,
        discoveryRetry: retryState(),
        validation: emptyConnectionValidation(),
        roles: { throughput: roleState(), latency: roleState() },
      };
      this.#servers.set(server.id, state);
      this.#publish(state);
    }
  }
  credentials(id: string): ServerCredentials | undefined {
    return this.#servers.get(id)?.credentials;
  }
  authorize(credentials: ServerCredentials): void {
    const state = this.#servers.get(credentials.server.id);
    if (!state || state.server.url !== credentials.server.url) return;
    this.#cancelServer(state);
    state.credentials = { ...credentials, server: state.server };
    state.discovery = undefined;
    state.discoveryError = undefined;
    state.discoveryRetry = retryState();
    state.validation = emptyConnectionValidation();
    for (const role of CONNECTION_ROLES) state.roles[role].retry = retryState();
    this.#publish(state);
    this.#schedule();
  }

  /** Changed intent cancels only its role. Unchanged peers and verified equivalent paths are retained. */
  select(selection: readonly { id: string; config: RunnerConfig }[]): void {
    this.#selected = selection.map(({ id }) => id);
    const configs = new Map(selection.map(({ id, config }) => [id, config]));
    for (const state of this.#servers.values()) {
      const config = configs.get(state.server.id) ?? null;
      if (!config) {
        if (state.config) {
          for (const role of CONNECTION_ROLES) {
            this.#cancelRole(state, role, false);
            state.roles[role].idle?.stop();
          }
          if (!this.#metadata) state.discoveryTask?.abort.abort(aborted());
        }
        state.config = null;
        continue;
      }
      state.config = config;
      for (const role of CONNECTION_ROLES) {
        const slot = state.roles[role];
        const key = connectionDraftRoleKey(config, role);
        if (slot.key === key) continue;
        slot.key = key;
        const reusable =
          !roleNeedsValidation(
            config,
            state.validation,
            role,
            state.discovery,
          ) &&
          (role !== "latency" || latencyPathNeeded(config));
        this.#cancelRole(state, role, !reusable);
        slot.retry = retryState();
        if (state.discoveryError) state.discoveryRetry = retryState();
        if (role === "latency" && !latencyPathNeeded(config)) {
          state.validation = {
            ...state.validation,
            latency: {
              ...state.validation.latency,
              selection: connectionSelection(config, role),
            },
          };
        } else if (
          roleNeedsValidation(config, state.validation, role, state.discovery)
        ) {
          state.validation = {
            ...state.validation,
            [role]: {
              selection: connectionSelection(config, role),
              state: "stale",
              path: null,
            },
          };
        } else {
          state.validation = {
            ...state.validation,
            [role]: {
              ...state.validation[role],
              selection: connectionSelection(config, role),
            },
          };
        }
      }
    }
    for (const state of this.#servers.values()) this.#publish(state);
    this.#refreshIdle();
    this.#schedule();
  }
  activity(enabled: boolean, idleServer: string | null): void {
    this.#enabled = enabled;
    this.#idleServer = enabled ? idleServer : null;
    this.#refreshIdle();
    this.#schedule();
  }
  metadata(enabled: boolean): void {
    this.#metadata = enabled;
    if (!enabled)
      for (const state of this.#servers.values()) {
        if (!state.config) {
          state.discoveryTask?.abort.abort();
          this.#publish(state);
        }
      }
    this.#schedule();
  }
  get metadataLoading(): boolean {
    return (
      this.#metadata &&
      [...this.#servers.values()].some(
        (state) => !state.config && !!state.discoveryTask,
      )
    );
  }
  requireAuthentication(id: string, message: string): void {
    const state = this.#servers.get(id);
    if (!state) return;
    this.#cancelServer(state);
    const error = new ServerAuthenticationRequired(state.server);
    error.message = message;
    state.discoveryError = error;
    this.#failed(state.discoveryRetry, error);
    state.validation = emptyConnectionValidation();
    this.#publish(state);
    this.#schedule();
  }
  paths(id: string, maxAgeMs = Infinity): PreparedPaths | null {
    const state = this.#servers.get(id);
    const paths =
      state?.config &&
      !state.discoveryError &&
      preparedPaths(
        state.config,
        state.discovery ?? null,
        state.validation,
        maxAgeMs,
      );
    return paths ? { ...paths, credentials: state!.credentials } : null;
  }
  ready(ids = this.#selected, maxAgeMs = Infinity): boolean {
    return (
      ids.length > 0 && ids.every((id) => this.paths(id, maxAgeMs) !== null)
    );
  }
  invalidate(ids = this.#selected, roles = CONNECTION_ROLES): void {
    for (const id of ids) {
      const state = this.#servers.get(id);
      if (!state) continue;
      for (const role of roles) {
        if (state.validation[role].state !== "verified") continue;
        const failedAdoption =
          !!state.roles[role].task && !!state.roles[role].idle;
        this.#cancelRole(state, role);
        state.roles[role].retry = retryState();
        if (failedAdoption)
          this.#failed(
            state.roles[role].retry,
            new Error("Idle connection lost"),
          );
        state.validation = {
          ...state.validation,
          [role]: { ...state.validation[role], state: "stale", path: null },
        };
      }
      this.#publish(state);
    }
    this.#schedule();
  }

  recover(serverId?: string): void {
    for (const state of this.#servers.values()) {
      if (serverId !== undefined && state.server.id !== serverId) continue;
      if (!state.discoveryRetry.authentication) state.discoveryRetry.at = 0;
      for (const role of CONNECTION_ROLES)
        if (!state.roles[role].retry.authentication)
          state.roles[role].retry.at = 0;
    }
    this.#schedule();
  }

  /** Matching checks join existing jobs; a caller can cancel only the jobs it creates. */
  async check(
    options: {
      ids?: string[];
      role?: ConnectionRole;
      force?: boolean;
      fresh?: boolean;
      signal?: AbortSignal;
    } = {},
  ): Promise<void> {
    if (this.#disposed) throw aborted();
    const ids = options.ids ?? [...this.#selected];
    if (!ids.length)
      throw new Error("Select a server before checking connections");
    const states = ids.map((id) => this.#servers.get(id)!);
    if (states.some((state) => !state?.config)) throw aborted();
    const keys = states.map((state) =>
      CONNECTION_ROLES.map((role) => state.roles[role].key).join("\n"),
    );
    if (options.force)
      for (const state of states)
        for (const role of this.#required(state))
          if (!options.role || role === options.role)
            this.#cancelRole(state, role);
    const work = Promise.allSettled(
      states.map((state) => this.#checkServer(state, options)),
    );
    const results = options.signal
      ? await abortable(work, options.signal)
      : await work;
    for (const result of results)
      if (result.status === "rejected" && result.reason?.name === "AbortError")
        throw result.reason;
    if (
      states.some(
        (state, i) =>
          this.#servers.get(state.server.id) !== state ||
          !state.config ||
          keys[i] !==
            CONNECTION_ROLES.map((role) => state.roles[role].key).join("\n"),
      )
    )
      throw aborted();
    const failures = states.filter(
      (state) =>
        !this.paths(
          state.server.id,
          options.fresh ? CONNECTION_FRESH_MS : Infinity,
        ),
    );
    if (failures.length)
      throw new Error(
        failures
          .map((state) => {
            const message =
              this.#view(state).readiness.message ??
              "Enabled measurements could not be prepared";
            return states.length > 1
              ? `${state.server.name}: ${message}`
              : message;
          })
          .join("; "),
      );
  }

  #current(state: ServerState): boolean {
    return !this.#disposed && this.#servers.get(state.server.id) === state;
  }
  #required(state: ServerState): ConnectionRole[] {
    return state.config
      ? CONNECTION_ROLES.filter(
          (role) => role !== "latency" || latencyPathNeeded(state.config!),
        )
      : [];
  }
  async #checkServer(
    state: ServerState,
    options: {
      role?: ConnectionRole;
      force?: boolean;
      fresh?: boolean;
      signal?: AbortSignal;
    },
  ): Promise<void> {
    const keys = CONNECTION_ROLES.map((role) => state.roles[role].key).join(
      "\n",
    );
    const discovery = await this.#discover(
      state,
      !!options.fresh,
      !!options.force,
      options.signal,
    );
    options.signal?.throwIfAborted();
    if (
      !state.config ||
      !this.#current(state) ||
      keys !== CONNECTION_ROLES.map((role) => state.roles[role].key).join("\n")
    )
      throw aborted();
    const config = state.config;
    const roles = this.#required(state).filter((role) => {
      if (options.force && (!options.role || role === options.role))
        return true;
      if (
        options.role &&
        role !== options.role &&
        state.validation[role].state === "failed"
      )
        return false;
      return (
        roleNeedsValidation(config, state.validation, role, discovery) ||
        (options.fresh &&
          Date.now() - state.validation[role].path!.verifiedAt >
            CONNECTION_FRESH_MS)
      );
    });
    const results = await Promise.allSettled(
      roles.map((role) => this.#probe(state, role, options.signal)),
    );
    for (const result of results)
      if (result.status === "rejected" && result.reason?.name === "AbortError")
        throw result.reason;
  }
  async #discover(
    state: ServerState,
    fresh: boolean,
    force = false,
    owner?: AbortSignal,
  ): Promise<TransportDiscovery> {
    if (state.discoveryTask) return state.discoveryTask.promise;
    if (
      state.discovery &&
      !state.discoveryError &&
      !force &&
      (!fresh || Date.now() - state.discovery.fetchedAt <= CONNECTION_FRESH_MS)
    )
      return state.discovery;
    const task: Operation<TransportDiscovery> = {
      abort: new AbortController(),
      promise: null!,
    };
    const cancel = () => task.abort.abort(owner?.reason);
    owner?.addEventListener("abort", cancel, { once: true });
    if (owner?.aborted) cancel();
    state.discoveryTask = task;
    state.discoveryError = undefined;
    const current = () =>
      this.#current(state) &&
      state.discoveryTask === task &&
      !task.abort.signal.aborted;
    task.promise = this.#network(
      () => (state.config ? 0 : 1),
      task.abort.signal,
      5000,
      async (signal) => {
        const discovery = await this.#deps.discover(signal, state.credentials);
        signal.throwIfAborted();
        if (!current()) throw aborted();
        const changedGeneration =
          state.discovery &&
          state.discovery.generation !== discovery.generation;
        state.discovery = discovery;
        state.discoveryError = undefined;
        state.discoveryRetry = retryState();
        state.server = {
          ...state.server,
          name: discovery.server.name || state.server.name,
          location: discovery.server.location,
        };
        state.credentials = { ...state.credentials, server: state.server };
        for (const role of CONNECTION_ROLES) {
          if (
            changedGeneration ||
            (state.validation[role].path &&
              state.validation[role].path!.generation !== discovery.generation)
          ) {
            this.#cancelRole(state, role);
            state.validation = {
              ...state.validation,
              [role]: { ...state.validation[role], state: "stale", path: null },
            };
            state.roles[role].retry = retryState();
          }
        }
        return discovery;
      },
    )
      .catch((error) => {
        if (current()) {
          state.discoveryError = error;
          this.#failed(state.discoveryRetry, error);
        }
        throw error;
      })
      .finally(() => {
        owner?.removeEventListener("abort", cancel);
        if (state.discoveryTask === task) {
          state.discoveryTask = undefined;
          this.#publish(state);
          this.#schedule();
        }
      });
    this.#publish(state);
    return task.promise;
  }
  #probe(
    state: ServerState,
    role: ConnectionRole,
    owner?: AbortSignal,
  ): Promise<void> {
    const slot = state.roles[role];
    if (slot.task) return slot.task.promise;
    const config = state.config!;
    const discovery = state.discovery!;
    const key = slot.key;
    const task: Operation<void> = {
      abort: new AbortController(),
      promise: null!,
    };
    const cancel = () => task.abort.abort(owner?.reason);
    owner?.addEventListener("abort", cancel, { once: true });
    if (owner?.aborted) cancel();
    slot.task = task;
    this.#stopIdle(slot);
    state.validation = {
      ...state.validation,
      [role]: {
        selection: connectionSelection(config, role),
        state: "checking",
        path: null,
      },
    };
    const current = () =>
      this.#current(state) &&
      !!state.config &&
      slot.task === task &&
      slot.key === key &&
      state.discovery?.generation === discovery.generation &&
      !task.abort.signal.aborted;
    task.promise = this.#network(
      () => 0,
      task.abort.signal,
      12000,
      async (signal) => {
        const result = await this.#deps.prepare(
          config,
          state.validation,
          [role],
          signal,
          state.credentials,
          discovery,
        );
        if (signal.aborted || !current()) {
          result.idle?.stop();
          throw signal.reason ?? aborted();
        }
        // The caller owns exactly one role, regardless of fields returned by the adapter.
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
        if (role === "latency" && result.idle && state.server.id === "self") {
          slot.idle = result.idle;
          const monitor = result.idle;
          monitor.onEvent = (event) => {
            if (
              slot.idle === monitor &&
              state.config &&
              this.#idleServer === state.server.id &&
              this.#current(state)
            )
              this.#deps.idleEvent(state.server.id, event);
          };
        } else result.idle?.stop();
      },
    )
      .catch((error) => {
        if (current()) {
          this.#failed(slot.retry, error);
          state.validation = {
            ...state.validation,
            [role]: {
              selection: connectionSelection(config, role),
              state: "failed",
              path: null,
              message: connectionFailureMessage(error),
            },
          };
        }
        throw error;
      })
      .finally(() => {
        owner?.removeEventListener("abort", cancel);
        if (slot.task === task) {
          slot.task = undefined;
          if (
            task.abort.signal.aborted &&
            state.validation[role].state === "checking"
          )
            state.validation = {
              ...state.validation,
              [role]: { ...state.validation[role], state: "stale", path: null },
            };
          this.#publish(state);
          this.#refreshIdle();
          this.#schedule();
        }
      });
    this.#publish(state);
    return task.promise;
  }
  #failed(retry: Retry, error: unknown): void {
    retry.authentication = authenticationFailure(error);
    retry.at = retry.authentication
      ? Infinity
      : Date.now() + connectionFailureBackoff(++retry.attempts);
  }
  #view(state: ServerState): ServerConnectionView {
    const roles = this.#required(state);
    const capability =
      state.config && uploadCapabilityFailure(state.config, state.discovery);
    const failed = roles.find(
      (role) => state.validation[role].state === "failed",
    );
    const auth =
      (!!state.discoveryError && state.discoveryRetry.authentication) ||
      roles.some((role) => state.roles[role].retry.authentication);
    const message = state.discoveryError
      ? connectionFailureMessage(state.discoveryError)
      : capability || (failed && state.validation[failed].message);
    const paths = this.paths(state.server.id);
    const checking =
      !!state.discoveryTask || roles.some((role) => !!state.roles[role].task);
    let validation = state.validation;
    if (state.discoveryTask || state.discoveryError) {
      validation = { ...validation };
      for (const role of roles)
        validation = {
          ...validation,
          [role]: {
            selection: connectionSelection(state.config!, role),
            state: state.discoveryTask ? "checking" : "failed",
            path: null,
            ...(message ? { message } : {}),
          },
        };
    }
    return {
      server: state.server,
      discovery: state.discovery,
      validation,
      metadataChecking: !state.config && !!state.discoveryTask,
      readiness: {
        state: auth
          ? "sign-in"
          : message
            ? "failed"
            : checking
              ? "checking"
              : paths
                ? "ready"
                : "unchecked",
        ...(message ? { message } : {}),
        ...(paths
          ? {
              checkedAt: Math.min(
                paths.throughput.verifiedAt,
                paths.latency?.verifiedAt ?? Infinity,
              ),
            }
          : {}),
      },
    };
  }
  #publish(state: ServerState): void {
    if (this.#current(state)) this.#deps.changed(this.#view(state));
  }
  #stopIdle(slot: RoleState): void {
    const idle = slot.idle;
    slot.idle = undefined;
    idle?.stop();
  }
  #cancelRole(
    state: ServerState,
    role: ConnectionRole,
    discardIdle = true,
  ): void {
    const slot = state.roles[role];
    const task = slot.task;
    slot.task = undefined;
    task?.abort.abort(aborted());
    if (discardIdle) this.#stopIdle(slot);
    if (state.validation[role].state === "checking")
      state.validation = {
        ...state.validation,
        [role]: { ...state.validation[role], state: "stale", path: null },
      };
  }
  #cancelServer(state: ServerState): void {
    const task = state.discoveryTask;
    state.discoveryTask = undefined;
    task?.abort.abort(aborted());
    for (const role of CONNECTION_ROLES) this.#cancelRole(state, role);
  }
  #refreshIdle(): void {
    for (const state of this.#servers.values()) {
      const idle = state.roles.latency.idle;
      if (!idle) continue;
      if (
        this.#enabled &&
        !state.discoveryError &&
        state.config &&
        state.server.id === this.#idleServer &&
        !roleNeedsValidation(
          state.config,
          state.validation,
          "latency",
          state.discovery,
        )
      )
        idle.start();
      else idle.stop();
    }
  }
  #schedule(): void {
    clearTimeout(this.#timer);
    this.#timer = undefined;
    if (!this.#enabled || this.#disposed) return;
    let at = Infinity;
    for (const state of this.#servers.values()) {
      if (state.discoveryTask) continue;
      if (!state.config) {
        if (
          this.#metadata &&
          (!state.discovery ||
            Date.now() - state.discovery.fetchedAt > CONNECTION_FRESH_MS)
        )
          at = Math.min(at, state.discoveryRetry.at);
      } else if (!state.discovery || state.discoveryError)
        at = Math.min(at, state.discoveryRetry.at);
      else
        for (const role of this.#required(state)) {
          if (
            !state.roles[role].task &&
            roleNeedsValidation(
              state.config,
              state.validation,
              role,
              state.discovery,
            )
          )
            at = Math.min(at, state.roles[role].retry.at);
        }
    }
    if (!Number.isFinite(at)) return;
    this.#timer = setTimeout(
      () => {
        this.#timer = undefined;
        this.#pump();
      },
      Math.max(0, at - Date.now()),
    );
  }
  #pump(): void {
    if (!this.#enabled || this.#disposed) return;
    for (const state of this.#servers.values()) {
      if (state.discoveryTask) continue;
      if (!state.config) {
        if (
          this.#metadata &&
          state.discoveryRetry.at <= Date.now() &&
          (!state.discovery ||
            Date.now() - state.discovery.fetchedAt > CONNECTION_FRESH_MS)
        )
          void this.#discover(state, true).catch(() => {});
      } else if (!state.discovery || state.discoveryError) {
        if (state.discoveryRetry.at <= Date.now())
          void this.#checkServer(state, {}).catch(() => {});
      } else
        for (const role of this.#required(state)) {
          if (
            !state.roles[role].task &&
            state.roles[role].retry.at <= Date.now() &&
            roleNeedsValidation(
              state.config,
              state.validation,
              role,
              state.discovery,
            )
          )
            void this.#probe(state, role).catch(() => {});
        }
    }
    this.#schedule();
  }
  async #network<T>(
    priority: () => number,
    owner: AbortSignal,
    timeoutMs: number,
    run: (signal: AbortSignal) => Promise<T>,
  ): Promise<T> {
    const release = await this.#acquire(priority, owner);
    try {
      return await withinBudget(owner, timeoutMs, run);
    } finally {
      release();
    }
  }

  #acquire(priority: () => number, signal: AbortSignal): Promise<() => void> {
    return new Promise((resolve, reject) => {
      if (signal.aborted) {
        reject(signal.reason);
        return;
      }
      const abort = () => {
        this.#queue = this.#queue.filter((queued) => queued !== item);
        reject(signal.reason);
      };
      const item = {
        priority,
        start: () => {
          signal.removeEventListener("abort", abort);
          this.#active++;
          resolve(() => {
            this.#active--;
            this.#drain();
          });
        },
      };
      signal.addEventListener("abort", abort, { once: true });
      this.#queue.push(item);
      this.#drain();
    });
  }
  #drain(): void {
    this.#queue.sort((a, b) => a.priority() - b.priority());
    while (this.#active < 2 && this.#queue.length) this.#queue.shift()!.start();
  }
  #stop(): void {
    clearTimeout(this.#timer);
    this.#timer = undefined;
    for (const state of this.#servers.values()) this.#cancelServer(state);
  }
  dispose(): void {
    this.#disposed = true;
    this.#stop();
    this.#servers.clear();
    this.#selected = [];
  }
}
