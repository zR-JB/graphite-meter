// One server's connection evidence between runs, each fact with its own backoff, and the Settings view of it.
import {
  batch,
  computed,
  effect,
  onCleanup,
  scope,
  signal,
  untrack,
  type Read,
  type Signal,
} from "../../lib/signal";
import type { CatalogEntry } from "../../protocol/documents";
import {
  identityOf,
  ROLES,
  type Discovery,
  type LatencyPath,
  type OpenProbeChannel,
  type PreparedServer,
  type ProbeChannel,
  type ProbeChannelEvents,
  type ProbeOutcome,
  type Role,
  type ServerAuth,
  type ServerIdentity,
  type ThroughputPath,
} from "../types";
import {
  checkLatency,
  checkThroughput,
  discover,
  failureOf,
  withinBudget,
  type CheckContext,
  type CheckedBus,
  type Failure,
} from "./checks";
import type { Limiter } from "./limiter";
import type { PageConfig, PageSession } from "./page";
import { isLoopback, selectionText, stillValid, type Selection } from "./paths";

/** Start runs on evidence at most this old when it judged it. A reselected server keeps discovery this old. */
export const FRESH_MS = 120_000;
/** An unmonitored selected server re-reads its discovery this often, so a dead peer never stays ready. */
const LIVENESS_MS = 5_000;
/** A monitored one re-reads it this often: its bus proves liveness, not an unchanged discovery behind a proxy. */
const MONITORED_MS = 60_000;
const DISCOVERY_BUDGET_MS = 5_000;
const ROLE_BUDGET_MS = 12_000;
const IDLE_INTERVAL_MS = 1_000;
const OFFLINE = "This device is offline";
const backoffMs = (attempts: number) => Math.min(60_000, 5_000 * 2 ** (attempts - 1));

export type Readiness = "unchecked" | "checking" | "verified" | "sign-in" | "failed";

interface RoleView<P> {
  /** The saved choice this role answers. */
  selection: string;
  state: "unchecked" | "checking" | "verified" | "failed";
  path: P | null;
  message: string | null;
}

export interface ServerView {
  server: ServerIdentity;
  discovery: Discovery | null;
  readiness: Readiness;
  message: string | null;
  /** A sign-in is a reason no connection check can clear. */
  blocked: string | null;
  throughput: RoleView<ThroughputPath>;
  latency: RoleView<LatencyPath>;
  /** Whether the server can run the current settings on its evidence, of any age. */
  ready: boolean;
  /** Settings is discovering this unselected server. */
  metadataChecking: boolean;
  /** An idle monitor probes this server's latency. Its outcomes reach `idleLatency`. */
  idle: boolean;
  /** An idle monitor last said that replies flow or that its bus went quiet. Null before one spoke. */
  connectivity: "connected" | "offline" | null;
}

interface CheckOptions {
  /** A retry re-reads discovery and redoes the role checks (all, or only `role`), whatever their backoff. */
  force?: boolean;
  role?: Role;
  /** Start and resume re-read discovery and re-check what failed or was verified before this page-ms instant. */
  since?: number;
  signal?: AbortSignal;
}

export interface ConnectionEnv {
  page: PageConfig;
  session: PageSession;
  limiter: Limiter;
  openProbeChannel: OpenProbeChannel;
  active: Read<boolean>;
  online: Read<boolean>;
  /** Settings is open, so unselected servers are discovered for their names and limits. */
  metadata: Read<boolean>;
  /** The server whose latency the console shows between runs. */
  latencyServer: Read<string | null>;
  selections: Record<Role, Read<Selection>>;
  latencyNeeded: Read<boolean>;
  idleLatency(id: string, outcomes: readonly ProbeOutcome[]): void;
  now(): number;
}

export interface Connection {
  readonly entry: CatalogEntry;
  readonly auth: ServerAuth;
  readonly view: Read<ServerView>;
  /** Without options, does what has fallen due. It never rejects, and outcomes land in the view. */
  check(options?: CheckOptions): Promise<void>;
  /** The server ready to run on evidence verified at or after `since` (page ms), or null. */
  prepared(since: number): PreparedServer | null;
  /** Verified roles become unchecked, e.g. after a run failed on them. */
  invalidate(roles: readonly Role[]): void;
  requireSignIn(message?: string): void;
  /** The network or the page came back, so retryable backoffs end and stale evidence is checked again. */
  resume(): void;
  dispose(): void;
}

type Task = { abort: AbortController; quiet: boolean; promise: Promise<void> };
/** A failure and when to retry it. `due` is Infinity when only the user or a changed server can clear it. */
type Fault = { failure: Failure; attempts: number; due: number };
type Outcome<P> =
  | { state: "none" }
  | { state: "verified"; path: P }
  | { state: "failed"; fault: Fault; selection: string; generation: string };
interface RoleCheck<P> {
  outcome: Signal<Outcome<P>>;
  task: Signal<Task | null>;
}

export function createConnection(
  entry: CatalogEntry,
  auth: ServerAuth,
  selected: Read<boolean>,
  env: ConnectionEnv,
): Connection {
  const [connection, dispose] = scope(() => connect(entry, auth, selected, env));
  return { ...connection, dispose };
}

function connect(
  entry: CatalogEntry,
  auth: ServerAuth,
  selected: Read<boolean>,
  env: ConnectionEnv,
): Omit<Connection, "dispose"> {
  const { now, page } = env;
  const context: CheckContext = { entry, auth, page, openProbeChannel: env.openProbeChannel, now };
  const discovery = signal<Discovery | null>(null);
  /** The last discovery failure is kept through a loud re-read, which only hides it. */
  const fault = signal<Fault | null>(null);
  const discovering = signal<Task | null>(null);
  const connectivity = signal<ServerView["connectivity"]>(null);
  const roleCheck = <P>(): RoleCheck<P> => ({
    outcome: signal<Outcome<P>>({ state: "none" }),
    task: signal(null),
  });
  const throughput = roleCheck<ThroughputPath>();
  const latency = roleCheck<LatencyPath>();
  const roles = { throughput, latency };
  const eachRole = (
    fn: <P extends ThroughputPath | LatencyPath>(check: RoleCheck<P>, role: Role) => void,
  ) => {
    fn(throughput, "throughput");
    fn(latency, "latency");
  };
  let disposed = false;
  /** The bus the latest latency check proved, for the idle monitor to take over. */
  let handed: CheckedBus | null = null;

  // The browser's offline word is advisory: a link without a gateway reads offline yet reaches its server.
  const loopback = isLoopback(new URL(entry.url).hostname);
  const offline = computed(() => !loopback && !env.online());
  const required = (role: Role) => selected() && (role === "throughput" || env.latencyNeeded());
  const name = computed(() => discovery()?.server.name || entry.name);
  const location = computed(() => {
    const known = discovery();
    return known ? known.server.location : entry.location;
  });
  const identity = computed(() => identityOf({ ...entry, name: name(), location: location() }));

  const validPath = <P extends ThroughputPath | LatencyPath>(role: Role, check: RoleCheck<P>) =>
    computed((): P | null => {
      const outcome = check.outcome();
      const known = discovery();
      const fits =
        outcome.state === "verified" &&
        known &&
        stillValid(outcome.path, known, role, env.selections[role](), page);
      return fits ? outcome.path : null;
    });
  const valid = {
    throughput: validPath("throughput", throughput),
    latency: validPath("latency", latency),
  };
  /** The role's failure while it still answers the current choice on the current discovery. */
  const failureHere = (role: Role): Fault | null => {
    const outcome = roles[role].outcome();
    const known = discovery();
    return outcome.state === "failed" &&
      known?.generation === outcome.generation &&
      selectionText(env.selections[role]()) === outcome.selection
      ? outcome.fault
      : null;
  };

  /** Gives the paths a run may use, verified at or after `since`, or null while anything stands in the way. */
  function paths(since: number): Pick<PreparedServer, "throughput" | "latency"> | null {
    if (!selected() || !discovery() || fault()) return null;
    const throughputPath = valid.throughput();
    if (!throughputPath || throughputPath.verifiedAt < since) return null;
    if (!env.latencyNeeded()) return { throughput: throughputPath, latency: null };
    const latencyPath = valid.latency();
    return latencyPath && latencyPath.verifiedAt >= since
      ? { throughput: throughputPath, latency: latencyPath }
      : null;
  }

  const watchedPath = computed(() =>
    env.latencyServer() === entry.id && env.active() && required("latency") && !fault()
      ? valid.latency()
      : null,
  );

  // These say what falls due, in page ms. Infinity means nothing, or what waits on the user.
  const roleDue = (role: Role): number => {
    if (!required(role) || roles[role].task() || valid[role]()) return Infinity;
    return failureHere(role)?.due ?? 0;
  };
  const discoveryDue = computed((): number => {
    if (discovering() || (!selected() && !env.metadata())) return Infinity;
    const failed = fault();
    if (failed) return failed.due;
    const known = discovery();
    if (!known) return 0;
    if (!selected()) return known.readAt + FRESH_MS;
    return known.readAt + (watchedPath() ? MONITORED_MS : LIVENESS_MS);
  });
  const signedOut = () => auth.left() <= 0 && !fault.peek()?.failure.signIn;
  const dueAt = computed((): number => {
    let at = discoveryDue();
    // A grant's end falls due on the page clock. Its wall-clock end is caught where anything is checked.
    if (!fault()?.failure.signIn) at = Math.min(at, now() + auth.left());
    if (discovery() && !fault() && !discovering())
      for (const role of ROLES) at = Math.min(at, roleDue(role));
    return at;
  });

  function cancel(slot: Signal<Task | null>): void {
    const task = slot.peek();
    if (!task) return;
    slot.set(null);
    task.abort.abort(new DOMException("Connection selection changed", "AbortError"));
  }
  function cancelAll(): void {
    batch(() => {
      cancel(discovering);
      eachRole((check) => cancel(check.task));
    });
  }

  function faultAfter(previous: Fault | null, error: unknown): Fault {
    const failure = failureOf(error, name.peek());
    // The page's own login ends for the whole page; the app navigates to sign-in.
    if (failure.signIn && auth.kind === "session") env.session.end("expired");
    if (failure.retry !== "retryable") return { failure, attempts: 0, due: Infinity };
    const attempts = (previous?.failure.retry === "retryable" ? previous.attempts : 0) + 1;
    return { failure, attempts, due: now() + backoffMs(attempts) };
  }

  /** Runs one check owning `slot` after a limiter slot, within its budget; an outcome lands only while it is current. */
  function job<T>(
    slot: Signal<Task | null>,
    quiet: boolean,
    budgetMs: number,
    owner: AbortSignal | undefined,
    run: (signal: AbortSignal) => Promise<T>,
    succeeded: (value: T) => void,
    failed: (error: unknown) => void,
  ): Promise<void> {
    const abort = new AbortController();
    const signal = owner ? AbortSignal.any([abort.signal, owner]) : abort.signal;
    const settle = (apply: () => void) =>
      batch(() => {
        if (slot.peek()?.abort !== abort) return;
        if (!disposed && !signal.aborted) apply();
        slot.set(null);
      });
    const promise = (async () => {
      let release: (() => void) | undefined;
      try {
        // The budget starts once a slot is granted, so queueing never spends it.
        release = await env.limiter.acquire(entry.url, () => (selected.peek() ? 0 : 1), signal);
        const value = await withinBudget(signal, budgetMs, run);
        settle(() => succeeded(value));
      } catch (error) {
        settle(() => failed(error));
      } finally {
        release?.();
      }
    })();
    slot.set({ abort, quiet, promise });
    return promise;
  }

  function rediscover(quiet: boolean, owner?: AbortSignal): Promise<void> {
    return job(
      discovering,
      quiet,
      DISCOVERY_BUDGET_MS,
      owner,
      (signal) => discover(context, signal),
      (next) => {
        const previous = discovery.peek();
        const same = previous?.generation === next.generation;
        // A restarted server answers for its new generation; checks against the old one are moot.
        if (previous && !same) eachRole((check) => cancel(check.task));
        // Automatic ranks by the hop first seen for a generation, so a late or upgraded one never moves its head.
        discovery.set(
          same && previous.protocol !== null ? { ...next, protocol: previous.protocol } : next,
        );
        fault.set(null);
      },
      (error) => fault.set(faultAfter(fault.peek(), error)),
    );
  }

  function verify<P>(
    check: RoleCheck<P>,
    known: Discovery,
    selection: Selection,
    owner: AbortSignal | undefined,
    run: (signal: AbortSignal) => Promise<{ path: P; bus: CheckedBus | null }>,
  ): Promise<void> {
    const text = selectionText(selection);
    return job(
      check.task,
      false,
      ROLE_BUDGET_MS,
      owner,
      run,
      ({ path, bus }) => {
        check.outcome.set({ state: "verified", path });
        // The idle monitor takes over the bus that proved its path, in this flush. No other bus outlives its check.
        if (bus && watchedPath.peek() === path) handed = bus;
        else bus?.dispose();
      },
      (error) => {
        const previous = check.outcome.peek();
        const same =
          previous.state === "failed" &&
          previous.selection === text &&
          previous.generation === known.generation;
        const fault = faultAfter(same ? previous.fault : null, error);
        check.outcome.set({
          state: "failed",
          fault,
          selection: text,
          generation: known.generation,
        });
      },
    );
  }

  function probe(role: Role, owner?: AbortSignal): Promise<void> {
    const running = roles[role].task.peek();
    if (running) return running.promise;
    const known = discovery.peek()!;
    const selection = env.selections[role].peek();
    return role === "throughput"
      ? verify(throughput, known, selection, owner, async (signal) => ({
          path: await checkThroughput(context, known, selection, signal),
          bus: null,
        }))
      : verify(latency, known, selection, owner, (signal) =>
          checkLatency(context, known, selection, signal),
        );
  }

  async function check({
    force = false,
    since,
    role,
    signal: owner,
  }: CheckOptions = {}): Promise<void> {
    if (disposed) return;
    if (signedOut()) return requireSignIn();
    const fresh = since !== undefined;
    const forced = (each: Role) => force && (!role || each === role);
    batch(() =>
      eachRole((check, each) => {
        if (forced(each)) cancel(check.task);
      }),
    );
    // A retry is seen to re-read, rather than joining a quiet liveness read.
    if (force && discovering.peek()?.quiet) cancel(discovering);
    const running = discovering.peek();
    const failed = fault.peek();
    const reread =
      force ||
      (fresh ? !failed || failed.failure.retry === "retryable" : discoveryDue.peek() <= now());
    // A selected server's own re-read keeps showing what it knew, without a checking flash.
    const quiet =
      !force && !fresh && selected.peek() && (discovery.peek() !== null || failed !== null);
    if (running) await running.promise;
    else if (reread) await rediscover(quiet, owner);
    if (disposed || owner?.aborted || !selected.peek() || fault.peek() || !discovery.peek()) return;
    const at = now();
    const stale = since ?? at - FRESH_MS;
    const due = ROLES.filter((each) =>
      untrack(() => {
        if (!required(each)) return false;
        if (forced(each) || roles[each].task()) return true;
        const path = valid[each]();
        if (path) return (force || fresh) && path.verifiedAt < stale;
        const failure = failureHere(each);
        // Retrying one role leaves another role's failure alone.
        if (!failure) return true;
        return (
          !role && failure.failure.retry === "retryable" && (force || fresh || failure.due <= at)
        );
      }),
    );
    await Promise.all(due.map((each) => probe(each, owner)));
  }

  function invalidate(which: readonly Role[]): void {
    batch(() =>
      eachRole((check, role) => {
        if (!which.includes(role) || check.outcome.peek().state !== "verified") return;
        cancel(check.task);
        check.outcome.set({ state: "none" });
      }),
    );
  }

  function requireSignIn(message?: string): void {
    const failure: Failure = {
      message: message ?? `Sign in to ${name.peek()}`,
      retry: "needs-user",
      signIn: true,
    };
    if (auth.kind === "session") env.session.end("expired");
    batch(() => {
      cancelAll();
      fault.set({ failure, attempts: 0, due: Infinity });
    });
  }

  function resume(): void {
    if (signedOut()) return requireSignIn();
    const reset = (each: Fault): Fault =>
      each.failure.retry === "retryable" ? { ...each, attempts: 0, due: 0 } : each;
    batch(() => {
      const failed = fault.peek();
      if (failed) fault.set(reset(failed));
      eachRole((check) => {
        const outcome = check.outcome.peek();
        if (outcome.state === "failed")
          check.outcome.set({ ...outcome, fault: reset(outcome.fault) });
      });
    });
    if (selected.peek() && !watchedPath.peek() && env.active.peek())
      void check({ since: now() - FRESH_MS });
  }

  // A changed choice cancels only its own role's check. The old outcome stays, answering the old choice.
  for (const role of ROLES) {
    const intent = computed(() => (required(role) ? selectionText(env.selections[role]()) : null));
    effect(() => {
      intent();
      untrack(() => cancel(roles[role].task));
    });
  }
  effect(() => {
    if (!selected() && !env.metadata()) untrack(() => cancel(discovering));
  });
  // A server selected again never promotes evidence older than FRESH_MS.
  let wasSelected = false;
  effect(() => {
    const isSelected = selected();
    if (isSelected && !wasSelected)
      untrack(() => {
        const at = now();
        batch(() => {
          const known = discovery.peek();
          if (known && at - known.readAt > FRESH_MS) discovery.set(null);
          eachRole((check) => {
            const outcome = check.outcome.peek();
            if (outcome.state === "verified" && at - outcome.path.verifiedAt > FRESH_MS)
              check.outcome.set({ state: "none" });
          });
        });
      });
    wasSelected = isSelected;
  });
  // Paths proven before the network went away are no evidence for the one that comes back.
  let wasOnline = env.online.peek();
  effect(() => {
    const online = env.online();
    if (online === wasOnline) return;
    wasOnline = online;
    untrack(() => (online ? resume() : selected() && invalidate(ROLES)));
  });
  effect(() => {
    if (!env.active()) return;
    const at = dueAt();
    if (at === Infinity) return;
    let armed = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const fire = () => {
      if (!armed) return;
      // A coarse page clock (Firefox's) can still read short of `at` when its timer fires, so nothing is due yet.
      const wait = Math.ceil(at - now());
      if (wait > 0) return void (timer = setTimeout(fire, wait));
      armed = false;
      void check();
    };
    // What is due already starts before the page paints, so a row never flashes its unchecked state.
    if (at <= now()) queueMicrotask(fire);
    else fire();
    onCleanup(() => {
      armed = false;
      clearTimeout(timer);
    });
  });
  effect(() => {
    const path = watchedPath();
    if (!path) return;
    const checked = handed;
    handed = null;
    let current = true;
    let channel: ProbeChannel | undefined;
    onCleanup(() => {
      current = false;
      channel?.dispose();
    });
    // A bus that cannot even open is a path to prove again.
    const lost = () =>
      queueMicrotask(() => {
        if (current) invalidate(["latency"]);
      });
    const events: ProbeChannelEvents = {
      ready() {},
      outcomes(outcomes) {
        if (!current) return;
        env.idleLatency(entry.id, outcomes);
        // Timeouts alone prove the bus runs, not that the server answers.
        if (outcomes.some(({ timedOut }) => !timedOut)) connectivity.set("connected");
      },
      interrupted() {},
      // A bus that went quiet is one too, since a peer that died leaves Ready.
      stalled() {
        if (current) connectivity.set("offline");
        lost();
      },
      resumed() {},
      signInRequired() {
        queueMicrotask(() => {
          if (current) requireSignIn();
        });
      },
    };
    if (checked) {
      channel = checked.adopt(events);
      channel.pace(IDLE_INTERVAL_MS);
      return;
    }
    try {
      channel = env.openProbeChannel(
        { target: path.target, auth, intervalMs: IDLE_INTERVAL_MS, maxInFlight: 2 },
        events,
      );
      // Every idle probe counts, as there is no warmup.
      channel.measure();
    } catch {
      lost();
    }
  });
  onCleanup(() => {
    disposed = true;
    cancelAll();
  });

  // The view reads settled booleans, so a quiet re-read that starts and ends changes nothing it shows.
  const loud = computed(() => discovering()?.quiet === false);
  const busy = {
    throughput: computed(() => throughput.task() !== null),
    latency: computed(() => latency.task() !== null),
  };
  const view = computed((): ServerView => {
    const known = discovery();
    const failed = loud() ? null : fault();
    // A failure other than a sign-in reads as the device's when it says it is offline.
    const isOffline = offline();
    const words = (failure: Failure) => (isOffline && !failure.signIn ? OFFLINE : failure.message);
    // The discovery's failure stands before any path.
    const standing = failed ? words(failed.failure) : null;
    const shown = <P>(role: Role, path: P | null): RoleView<P> => {
      const selection = selectionText(env.selections[role]());
      const needed = required(role);
      if (needed && loud()) return { selection, state: "checking", path: null, message: null };
      if (needed && standing) return { selection, state: "failed", path: null, message: standing };
      if (needed && busy[role]())
        return { selection, state: "checking", path: null, message: null };
      if (path) return { selection, state: "verified", path, message: null };
      const failure = needed ? failureHere(role) : null;
      if (failure)
        return { selection, state: "failed", path: null, message: words(failure.failure) };
      return { selection, state: "unchecked", path: null, message: null };
    };
    const throughputView = shown("throughput", valid.throughput());
    const latencyView = shown("latency", valid.latency());
    const roleViews = ROLES.filter(required).map((role) =>
      role === "throughput" ? throughputView : latencyView,
    );
    const message = standing ?? roleViews.find((each) => each.message)?.message ?? null;
    const signIn =
      !!failed?.failure.signIn ||
      ROLES.some((role) => required(role) && !busy[role]() && failureHere(role)?.failure.signIn);
    const ready = paths(-Infinity) !== null;
    const checking = loud() || ROLES.some((role) => required(role) && busy[role]());
    return {
      server: identity(),
      discovery: known,
      readiness: signIn
        ? "sign-in"
        : message
          ? "failed"
          : checking
            ? "checking"
            : ready
              ? "verified"
              : "unchecked",
      message,
      blocked: signIn ? message : null,
      throughput: throughputView,
      latency: latencyView,
      ready,
      metadataChecking: !selected() && loud(),
      idle: watchedPath() !== null,
      connectivity: connectivity(),
    };
  });

  return {
    entry,
    auth,
    view,
    check,
    prepared: (since) =>
      untrack(() => {
        if (auth.left() <= 0) return null;
        const ready = paths(since);
        return ready && { server: identity(), auth, discovery: discovery()!, ...ready };
      }),
    invalidate,
    requireSignIn,
    resume,
  };
}
