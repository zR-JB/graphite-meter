import type {
  ConnectionRole,
  PreparedPaths,
  RunnerConfig,
  TransportDiscovery,
} from "./contract";
import type { IdleEvent } from "./real/latencyChannel";
import {
  BrowserOriginBlockedError,
  PreflightUnavailableError,
  type ConnectionPreparation,
  type discoverServer,
  type prepareConnections,
} from "./real/prepare";
import { causes, findCause, withinBudget } from "./abortable";
import type { ServerEntry } from "../servers/catalog";
import {
  ServerAuthenticationRequired,
  type ServerCredentials,
} from "../servers/credentials";
import type { originLimiter } from "../servers/originLimiter";
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
  type ServerView,
} from "./paths";

export interface ConnectionHost {
  discover: typeof discoverServer;
  prepare: typeof prepareConnections;
  limiter: ReturnType<typeof originLimiter>;
  /** Background checks and idle latency run only while this holds. */
  active(): boolean;
  metadata(): boolean;
  idle(id: string): boolean;
  publish(view: ServerView): void;
  idleEvent(id: string, event: IdleEvent): void;
}
export interface CheckOptions {
  force?: boolean;
  role?: ConnectionRole;
  /** Re-verify paths older than the freshness window, as a run start does. */
  fresh?: boolean;
  due?: boolean;
  signal?: AbortSignal;
}
type Backoff = { attempts: number; at: number; signIn: boolean };
interface Job<T> {
  backoff: Backoff;
  task?: { abort: AbortController; signal: AbortSignal; promise: Promise<T> };
}
interface Role extends Job<void> {
  key: string;
  idle?: NonNullable<ConnectionPreparation["idle"]>;
}

const NETWORK_FAILURE =
  /failed to fetch|fetch failed|network(?:error| request failed)|load failed|connection (?:refused|reset|lost)/i;
const backoff = (): Backoff => ({ attempts: 0, at: 0, signIn: false });
const superseded = () =>
  new DOMException("Connection selection changed", "AbortError");

function failureMessage(cause: unknown, server: ServerEntry): string {
  const authentication = findCause(cause, ServerAuthenticationRequired);
  if (authentication) return authentication.message;
  if (cause instanceof BrowserOriginBlockedError) return cause.message;
  if (cause instanceof PreflightUnavailableError)
    for (const { name, message } of causes(cause.cause))
      if (name === "NetworkError" || NETWORK_FAILURE.test(message))
        return server.url.startsWith("https://") &&
          location.protocol === "http:"
          ? "Server could not be reached. If it requires sign-in, open this interface over HTTPS."
          : "Server could not be reached";
  return cause instanceof DOMException && cause.name === "TimeoutError"
    ? "Connection check timed out"
    : "Connection check failed";
}

const sameView = (a: ServerView, b: ServerView) =>
  a.server === b.server &&
  a.discovery === b.discovery &&
  a.readiness === b.readiness &&
  a.message === b.message &&
  a.metadataChecking === b.metadataChecking &&
  CONNECTION_ROLES.every((role) =>
    (["state", "path", "message", "selection"] as const).every(
      (key) => a.validation[role][key] === b.validation[role][key],
    ),
  );

/** One server's connection evidence: discovery, both role checks, backoff, freshness, idle monitor and sign-in. */
export class ServerConnection {
  config: RunnerConfig | null = null;
  view!: ServerView;
  #discovery?: TransportDiscovery;
  #error?: unknown;
  #validation = emptyConnectionValidation();
  #discovering: Job<TransportDiscovery | null> = { backoff: backoff() };
  #roles: Record<ConnectionRole, Role> = {
    throughput: { key: "", backoff: backoff() },
    latency: { key: "", backoff: backoff() },
  };
  #timer?: ReturnType<typeof setTimeout>;
  #closed = false;

  constructor(
    public server: ServerEntry,
    public credentials: ServerCredentials,
    private readonly host: ConnectionHost,
  ) {
    this.#sync();
  }

  /** Null deselects. A changed role intent cancels only that role; expired evidence is never promoted. */
  select(config: RunnerConfig | null): void {
    this.config = config;
    if (!config) {
      for (const role of CONNECTION_ROLES) this.#cancelRole(role, false);
      if (!this.host.metadata()) this.#cancel(this.#discovering);
      return this.#sync();
    }
    if (
      this.#discovery &&
      Date.now() - this.#discovery.fetchedAt > CONNECTION_FRESH_MS
    ) {
      for (const role of CONNECTION_ROLES) this.#cancelRole(role, false);
      this.#discovery = undefined;
    }
    for (const role of CONNECTION_ROLES) {
      const slot = this.#roles[role];
      const key = connectionDraftRoleKey(config, role);
      const path = this.#validation[role].path;
      const expired =
        !!path && Date.now() - path.verifiedAt > CONNECTION_FRESH_MS;
      if (slot.key === key && !expired) continue;
      slot.key = key;
      const stale = expired || this.#needsCheck(role);
      const unused = role === "latency" && !latencyPathNeeded(config);
      this.#cancelRole(role, stale || unused);
      slot.backoff = backoff();
      if (this.#error) this.#discovering.backoff = backoff();
      const selection = connectionSelection(config, role);
      this.#setRole(
        role,
        !unused && stale
          ? { selection, state: "stale", path: null, message: undefined }
          : { selection },
      );
    }
    this.#sync();
  }

  /** Starts or joins discovery and the needed role checks; outcomes land in the view and never reject. */
  async check({
    force = false,
    role,
    fresh = false,
    due = false,
    signal,
  }: CheckOptions = {}): Promise<void> {
    if (force)
      for (const r of this.#required())
        if (!role || r === role) this.#cancelRole(r);
    const discovery = await this.#discover(
      fresh || !this.config,
      force,
      signal,
    );
    const config = this.config;
    if (!discovery || !config || signal?.aborted || this.#closed) return;
    const now = Date.now();
    const roles = this.#required().filter((r) => {
      const { state, path } = this.#validation[r];
      if (force && (!role || r === role)) return true;
      if (
        (role && r !== role && state === "failed") ||
        (due && this.#roles[r].backoff.at > now)
      )
        return false;
      return (
        roleNeedsValidation(config, this.#validation, r, discovery) ||
        (fresh && !!path && now - path.verifiedAt > CONNECTION_FRESH_MS)
      );
    });
    await Promise.all(
      roles.map((r) => this.#probe(r, config, discovery, signal)),
    );
  }

  paths(maxAgeMs = CONNECTION_FRESH_MS): PreparedPaths | null {
    if (!this.config || this.#expired() || this.#error) return null;
    const prepared = preparedPaths(
      this.config,
      this.#discovery ?? null,
      this.#validation,
      maxAgeMs,
    );
    return prepared && { ...prepared, credentials: this.credentials };
  }

  invalidate(roles: readonly ConnectionRole[]): void {
    for (const role of roles) {
      if (this.#validation[role].state !== "verified") continue;
      this.#cancelRole(role);
      this.#roles[role].backoff = backoff();
      this.#setRole(role, { state: "stale", path: null });
    }
    this.#sync();
  }

  requireSignIn(message?: string): void {
    this.#cancelAll();
    const error = new ServerAuthenticationRequired(this.server);
    if (message) error.message = message;
    this.#error = error;
    this.#failed(this.#discovering.backoff, error);
    this.#validation = emptyConnectionValidation();
    this.#sync();
  }

  /** Retries waiting on backoff run now; sign-in waits for approval. */
  resume(): void {
    for (const job of [this.#discovering, ...Object.values(this.#roles)])
      if (!job.backoff.signIn) job.backoff.at = 0;
    this.#sync();
  }

  wake(): void {
    this.#sync();
  }

  close(): void {
    this.#cancelAll();
    clearTimeout(this.#timer);
    this.#closed = true;
  }

  /** When the next background check is due; Infinity when none is. */
  dueAt(): number {
    const discovering = this.#discovering;
    let at =
      this.credentials.kind === "grant" && !discovering.backoff.signIn
        ? (this.credentials.expiresAt ?? 0)
        : Infinity;
    if (discovering.task) return at;
    if (!this.config) {
      const stale =
        !this.#discovery ||
        Date.now() - this.#discovery.fetchedAt > CONNECTION_FRESH_MS;
      if (this.host.metadata() && stale)
        at = Math.min(at, discovering.backoff.at);
    } else if (!this.#discovery || this.#error)
      at = Math.min(at, discovering.backoff.at);
    else
      for (const role of this.#required())
        if (!this.#roles[role].task && this.#needsCheck(role))
          at = Math.min(at, this.#roles[role].backoff.at);
    return at;
  }

  #due(): void {
    if (!this.host.active() || this.dueAt() > Date.now()) return this.#sync();
    if (this.#expired() && !this.#discovering.backoff.signIn)
      this.requireSignIn();
    else void this.check({ due: true });
  }

  #discover(
    fresh: boolean,
    force: boolean,
    owner?: AbortSignal,
  ): Promise<TransportDiscovery | null> {
    const job = this.#discovering;
    if (job.task && !job.task.signal.aborted) return job.task.promise;
    const known = this.#discovery;
    if (
      known &&
      !this.#error &&
      !force &&
      (!fresh || Date.now() - known.fetchedAt <= CONNECTION_FRESH_MS)
    )
      return Promise.resolve(known);
    this.#error = undefined;
    return this.#job(
      job,
      5000,
      owner,
      async (signal, live) => {
        const discovery = await this.host.discover(signal, this.credentials);
        if (!live()) throw signal.reason ?? superseded();
        const previous = this.#discovery;
        const restarted =
          !!previous && previous.generation !== discovery.generation;
        this.#discovery = discovery;
        job.backoff = backoff();
        this.server = {
          ...this.server,
          name: discovery.server.name || this.server.name,
          location: discovery.server.location,
        };
        this.credentials = { ...this.credentials, server: this.server };
        for (const role of CONNECTION_ROLES) {
          const path = this.#validation[role].path;
          if (!restarted && (!path || path.generation === discovery.generation))
            continue;
          this.#cancelRole(role);
          this.#setRole(role, { state: "stale", path: null });
          this.#roles[role].backoff = backoff();
        }
        return discovery;
      },
      (error) => {
        if (error !== undefined) {
          this.#error = error;
          this.#failed(job.backoff, error);
        }
        return null;
      },
    );
  }

  #probe(
    role: ConnectionRole,
    config: RunnerConfig,
    discovery: TransportDiscovery,
    owner?: AbortSignal,
  ) {
    const slot = this.#roles[role];
    if (slot.task && !slot.task.signal.aborted) return slot.task.promise;
    const selection = connectionSelection(config, role);
    this.#stopIdle(slot);
    this.#setRole(role, {
      selection,
      state: "checking",
      path: null,
      message: undefined,
    });
    return this.#job(
      slot,
      12000,
      owner,
      async (signal, live) => {
        const result = await this.host.prepare(
          config,
          this.#validation,
          [role],
          signal,
          this.credentials,
          discovery,
        );
        if (!live()) {
          result.idle?.stop();
          throw signal.reason ?? superseded();
        }
        const checked = result.validation[role];
        this.#setRole(role, checked);
        if (result.failure || checked.state !== "verified" || !checked.path) {
          result.idle?.stop();
          throw (
            result.failure ??
            new Error(checked.message ?? "Connection check failed")
          );
        }
        slot.backoff = backoff();
        if (result.idle) this.#adopt(slot, result.idle);
      },
      (error) => {
        if (error === undefined) return;
        this.#failed(slot.backoff, error);
        this.#setRole(role, {
          selection,
          state: "failed",
          path: null,
          message: failureMessage(error, this.server),
        });
      },
    );
  }

  /** Runs one origin-limited job; a failure goes to `fail` only while the job is still current. */
  #job<T>(
    job: Job<T>,
    timeoutMs: number,
    owner: AbortSignal | undefined,
    run: (signal: AbortSignal, live: () => boolean) => Promise<T>,
    fail: (error: unknown) => T,
  ): Promise<T> {
    const abort = new AbortController();
    const signal = owner
      ? AbortSignal.any([abort.signal, owner])
      : abort.signal;
    const live = () =>
      !this.#closed && job.task?.abort === abort && !signal.aborted;
    const work = async () => {
      const origin = new URL(this.server.url).origin;
      const release = await this.host.limiter.acquire(
        origin,
        () => (this.config ? 0 : 1),
        signal,
      );
      try {
        return await withinBudget(signal, timeoutMs, (inner) =>
          run(inner, live),
        );
      } finally {
        release();
      }
    };
    const promise = work()
      .catch((error) => fail(live() ? error : undefined))
      .finally(() => {
        if (job.task?.abort !== abort) return;
        job.task = undefined;
        this.#sync();
      });
    job.task = { abort, signal, promise };
    this.#sync();
    return promise;
  }

  #adopt(slot: Role, monitor: NonNullable<Role["idle"]>): void {
    slot.idle = monitor;
    monitor.onEvent = (event) => {
      if (
        slot.idle === monitor &&
        this.config &&
        !this.#closed &&
        this.host.idle(this.server.id)
      )
        this.host.idleEvent(this.server.id, event);
    };
  }

  #sync(): void {
    if (this.#closed) return;
    const view = this.#view();
    if (!this.view || !sameView(this.view, view)) {
      this.view = view;
      this.host.publish(view);
    }
    const idle = this.#roles.latency.idle;
    if (
      this.config &&
      this.host.idle(this.server.id) &&
      !this.#expired() &&
      !this.#error &&
      !this.#needsCheck("latency")
    )
      idle?.start();
    else idle?.stop();
    clearTimeout(this.#timer);
    const at = this.host.active() ? this.dueAt() : Infinity;
    if (Number.isFinite(at))
      this.#timer = setTimeout(() => this.#due(), Math.max(0, at - Date.now()));
  }

  #view(): ServerView {
    const { config } = this;
    const expired = this.#expired();
    const roles = this.#required();
    const discovering = !!this.#discovering.task;
    const failed = roles.find(
      (role) => this.#validation[role].state === "failed",
    );
    const capability = config
      ? uploadCapabilityFailure(config, this.#discovery)
      : undefined;
    const message = expired
      ? new ServerAuthenticationRequired(this.server).message
      : this.#error
        ? failureMessage(this.#error, this.server)
        : capability || (failed && this.#validation[failed].message);
    let validation = this.#validation;
    // The view reports what a run could use now; the evidence itself stays for the next check.
    for (const role of roles) {
      const own = validation[role];
      const shown =
        discovering || this.#error
          ? {
              selection: own.selection,
              state: discovering ? ("checking" as const) : ("failed" as const),
              path: null,
              ...(message ? { message } : {}),
            }
          : role === "throughput" && capability
            ? { ...own, state: "failed" as const, message: capability }
            : own;
      validation = { ...validation, [role]: shown };
    }
    const signIn =
      expired ||
      (!!this.#error && this.#discovering.backoff.signIn) ||
      roles.some((role) => this.#roles[role].backoff.signIn);
    return {
      server: this.server,
      discovery: this.#discovery ?? null,
      validation,
      metadataChecking: !config && discovering,
      readiness: signIn
        ? "sign-in"
        : message
          ? "failed"
          : discovering || roles.some((role) => this.#roles[role].task)
            ? "checking"
            : this.paths()
              ? "ready"
              : "unchecked",
      ...(message ? { message } : {}),
    };
  }

  #required(): ConnectionRole[] {
    const { config } = this;
    return config
      ? CONNECTION_ROLES.filter(
          (role) => role !== "latency" || latencyPathNeeded(config),
        )
      : [];
  }
  #needsCheck(role: ConnectionRole): boolean {
    return (
      !!this.config &&
      roleNeedsValidation(this.config, this.#validation, role, this.#discovery)
    );
  }
  #expired(): boolean {
    return (
      this.credentials.kind === "grant" &&
      (this.credentials.expiresAt ?? 0) <= Date.now()
    );
  }
  #setRole(
    role: ConnectionRole,
    check: Partial<ConnectionValidation[ConnectionRole]>,
  ): void {
    this.#validation = {
      ...this.#validation,
      [role]: { ...this.#validation[role], ...check },
    };
  }
  #failed(backoff: Backoff, error: unknown): void {
    backoff.signIn = !!findCause(error, ServerAuthenticationRequired);
    backoff.at = backoff.signIn
      ? Infinity
      : Date.now() + connectionFailureBackoff(++backoff.attempts);
  }
  #cancel(job: Job<unknown>): void {
    const task = job.task;
    job.task = undefined;
    task?.abort.abort(superseded());
  }
  #stopIdle(slot: Role): void {
    const idle = slot.idle;
    slot.idle = undefined;
    idle?.stop();
  }
  #cancelRole(role: ConnectionRole, discardIdle = true): void {
    this.#cancel(this.#roles[role]);
    if (discardIdle) this.#stopIdle(this.#roles[role]);
    if (this.#validation[role].state === "checking")
      this.#setRole(role, { state: "stale", path: null });
  }
  #cancelAll(): void {
    this.#cancel(this.#discovering);
    for (const role of CONNECTION_ROLES) this.#cancelRole(role);
  }
}
