// The servers a test runs on, for the app layer: the operator's catalogue, the selection and its saved choice, each
// server's credentials and connection, the approval that grants a peer, and what Start runs on.
//
// `createServers(env)` returns signals the UI reads (catalogue, loading, loadError, selected, unresolved, views,
// approval, selectionState, blocker, stageLimit, streamPlanError, streams, latencyServer, metadataLoading) and the
// methods the app calls: load() at boot and retryCatalogue(); apply(ids); signIn(id) inside the click and
// cancelSignIn(); retry(id?, role?); showMetadata(open) with Settings; authorize(requiredMs, signal) then
// prepare(signal) at Start, and fits(totalMs) for a live change; recheck(failure) after a run; resume() when the page
// shows again; dispose().
import { object, string } from "../../lib/decode";
import { batch, computed, signal, type Read } from "../../lib/signal";
import {
  canonicalOrigin,
  catalog as catalogDocument,
  displayText,
  MAX_STAGE_LIMIT_MS,
  readJSON,
  type Catalog,
  type CatalogEntry,
} from "../../protocol/documents";
import { ROUTES } from "../../protocol/routes";
import { planned } from "../run/schedule";
import type { DroppedServer } from "../run/run";
import type {
  FailureReason,
  OpenProbeChannel,
  PreparedServer,
  ProbeOutcome,
  Role,
  ServerAuth,
  TestConfig,
} from "../types";
import { ApprovalLimitError, beginApproval, exchangeGrant, grantAuth, initialAuth, type Grant } from "./auth";
import { describeStreams, needsPings, planError, plannedActivities, type PlanServer } from "./budget";
import { createConnection, FRESH_MS, type Connection, type ConnectionEnv, type ServerView } from "./connection";
import { createLimiter } from "./limiter";
import { CoverageError, type PageConfig, type PageSession } from "./page";
import { allowsOrigin, parseSelection, portable, selectionText } from "./paths";

export interface ServersEnv {
  page: PageConfig;
  session: PageSession;
  /** The saved test settings, which the app owns. */
  config: Read<TestConfig>;
  /** Rewrites the saved path choices when a choice must apply to every selected server. */
  setTransports(transports: TestConfig["transports"]): void;
  /** Checks and their timers may run: the app is booted and visible, and no run or start is pending. */
  active: Read<boolean>;
  online: Read<boolean>;
  /** The server whose latency the console pins, if any. */
  latencyPin: Read<string | null>;
  openProbeChannel: OpenProbeChannel;
  /** The idle monitor's outcomes for the shown latency server between runs. */
  idleLatency(id: string, outcomes: readonly ProbeOutcome[]): void;
  now(): number;
}

/** A saved selection row: an ID bound to the origin it named, so an ID never silently moves to another server. */
export interface SavedServer {
  id: string;
  url: string;
}

/** A peer's sign-in, one at a time across all servers. */
export type Approval =
  | { state: "idle" }
  /** The window is reserved; the challenge is being made. */
  | { state: "preparing"; id: string }
  | { state: "waiting"; id: string; url: string; code: string }
  | { state: "failed"; id: string; url: string; code: string }
  /** The remote login holds all its grants: it must be renewed there first. */
  | { state: "limit"; id: string; renewUrl: string };

export interface Servers {
  catalogue: Read<Catalog | null>;
  loading: Read<boolean>;
  /** Why the latest catalogue load failed, or "". */
  loadError: Read<string>;
  /** In catalogue order. */
  selected: Read<readonly string[]>;
  /** Saved rows whose server changed: the user must choose again. */
  unresolved: Read<readonly SavedServer[]>;
  views: Read<ReadonlyMap<string, ServerView>>;
  approval: Read<Approval>;
  selectionState: Read<"checking" | "verified" | "failed" | "unchecked">;
  /** The servers' reason Start cannot run, or "". */
  blocker: Read<string>;
  /** The longest stage every discovered selected server admits, and the server that sets it. */
  stageLimit: Read<{ ms: number; server: string | null }>;
  /** Why the planned stages cannot run on the selected servers' paths, or "" (also while any is unverified). */
  streamPlanError: Read<string>;
  /** The lane policy in words for Settings. */
  streams: Read<{ value: string; aside: string }>;
  /** The server whose latency the console shows: the pinned one while selected, else the first selected. */
  latencyServer: Read<string | null>;
  metadataLoading: Read<boolean>;
  load(): Promise<void>;
  retryCatalogue(): Promise<void>;
  /** False when the selection cannot change now; throws for a set that is not one to four catalogue servers. */
  apply(ids: readonly string[]): boolean;
  /** Call from the click itself: the sign-in window must open within the user's activation. */
  signIn(id: string): void;
  cancelSignIn(): void;
  retry(id?: string, role?: Role): Promise<void>;
  showMetadata(open: boolean): void;
  /** The page's login and every selected grant must cover `requiredMs`; a CoverageError carries the words to show. */
  authorize(requiredMs: number, signal: AbortSignal): Promise<void>;
  /** Re-reads every selected server's discovery and re-checks stale paths: what Start runs on, the servers left out
      with their reason, and whose latency leads. An AbortError means a reload or an approval replaced a connection. */
  prepare(signal: AbortSignal): Promise<{ servers: PreparedServer[]; dropped: DroppedServer[]; focus: string }>;
  /** Whether a run lasting `totalMs` from now ends before every login `authorize` checked. */
  fits(totalMs: number): boolean;
  /** A run failed on one server, or with null as a whole: a refused sign-in is required again, other failures send
      the failed paths back to be checked. */
  recheck(failure: { serverId: string; scope: Role; reason: FailureReason; message: string } | null): void;
  resume(): void;
  dispose(): void;
}

const IDLE: Approval = { state: "idle" };
const SAVED_KEY = "graphite-meter:server-selection:v1";
const MAX_SELECTED = 4;
const CATALOGUE_TIMEOUT_MS = 5_000;
const ROLES: readonly Role[] = ["throughput", "latency"];
const TRANSPORT_KEY = { throughput: "throughputTarget", latency: "latencyTarget" } as const;

const savedRow = object({ id: displayText(64, 1), url: string(1, 2048) });

/** `ids` in catalogue order. */
const inCatalogueOrder = (catalog: Catalog, ids: readonly string[]) =>
  catalog.servers.filter((entry) => ids.includes(entry.id)).map((entry) => entry.id);

/** The saved choice against the current catalogue. A row counts only while its ID keeps its origin, else it waits
    for the user; no choice, or anything malformed, means the operator's defaults. */
export function reconcile(catalog: Catalog, saved: unknown): { ids: string[]; unresolved: SavedServer[] } {
  const defaults = { ids: inCatalogueOrder(catalog, catalog.defaultSelection), unresolved: [] };
  if (!Array.isArray(saved) || saved.length < 1 || saved.length > MAX_SELECTED) return defaults;
  const origins = new Map(catalog.servers.map((entry) => [entry.id, entry.url]));
  const seen = new Set<string>();
  const ids: string[] = [];
  const unresolved: SavedServer[] = [];
  for (const row of saved) {
    let id: string;
    let url: string;
    try {
      ({ id, url } = savedRow(row));
      url = canonicalOrigin(url);
    } catch {
      return defaults;
    }
    if (seen.has(id)) return defaults;
    seen.add(id);
    if (origins.get(id) === url) ids.push(id);
    else unresolved.push({ id, url });
  }
  return { ids: inCatalogueOrder(catalog, ids), unresolved };
}

function readSaved(): unknown {
  try {
    const text = localStorage.getItem(SAVED_KEY);
    return text === null ? null : JSON.parse(text);
  } catch {
    return null;
  }
}

/** Blocked storage keeps the choice for this page only. */
function writeSaved(rows: SavedServer[]): void {
  try {
    localStorage.setItem(SAVED_KEY, JSON.stringify(rows));
  } catch {}
}

/** A window reserved inside the click, before the challenge exists: popup blockers allow nothing later. The opener
    link is cut at once, as `noopener` would return no handle to navigate or close. */
function reserveWindow(signal: AbortSignal): { navigate(url: string): void; close(): void } {
  let popup: Window | null = null;
  const close = () => {
    signal.removeEventListener("abort", close);
    try {
      popup?.close();
    } catch {
      // Cross-origin isolation may sever the handle; approval never depends on it.
    }
    popup = null;
  };
  try {
    popup = window.open("about:blank", "_blank", "popup,width=520,height=720");
    if (popup) popup.opener = null;
  } catch {
    close();
  }
  signal.addEventListener("abort", close, { once: true });
  return {
    navigate(url) {
      try {
        if (!signal.aborted) popup?.location.replace(url);
      } catch {
        // The visible sign-in link works when navigation is blocked.
        close();
      }
    },
    close,
  };
}

export function createServers(env: ServersEnv): Servers {
  const { page, config, now } = env;
  const catalogue = signal<Catalog | null>(null);
  const loading = signal(false);
  const loadError = signal("");
  const selected = signal<readonly string[]>([]);
  const unresolved = signal<readonly SavedServer[]>([]);
  const connections = signal<ReadonlyMap<string, Connection>>(new Map());
  const approval = signal(IDLE);
  const metadata = signal(false);
  // Grants outlive a catalogue reload while their server keeps its ID and origin; reloading the page drops them.
  const grants = new Map<string, { url: string; grant: Grant; auth: ServerAuth }>();
  const limiter = createLimiter();
  const lifetime = new AbortController();
  let loadRequest: AbortController | null = null;
  let flow: AbortController | null = null;
  /** The earliest monotonic end of the logins the last start authorized; null when none ends. */
  let budget: number | null = null;

  const choice = (role: Role) => {
    const text = computed(() => config().transports[TRANSPORT_KEY[role]]);
    return computed(() => parseSelection(text()));
  };
  const selections = { throughput: choice("throughput"), latency: choice("latency") };
  const activities = computed(() => plannedActivities(config()));
  const latencyNeeded = computed(() => activities().some(needsPings));
  const checkpoints = computed(() => planned(config(), "upload") || planned(config(), "bidirectional"));
  const representative = computed(() => selected()[0] ?? null);
  const latencyServer = computed(() => {
    const pin = env.latencyPin();
    return pin !== null && selected().includes(pin) ? pin : representative();
  });
  const views = computed(
    (): ReadonlyMap<string, ServerView> =>
      new Map([...connections()].map(([id, connection]) => [id, connection.view()])),
  );
  const selectedViews = () => selected().flatMap((id) => views().get(id) ?? []);
  const planServer = (view: ServerView): PlanServer | null =>
    view.throughput.path && {
      server: view.server,
      throughput: view.throughput.path,
      latency: latencyNeeded() ? view.latency.path : null,
    };

  const connectionEnv: ConnectionEnv = {
    page,
    session: env.session,
    limiter,
    openProbeChannel: env.openProbeChannel,
    active: computed(() => env.active() && !loading()),
    online: env.online,
    metadata,
    latencyServer,
    selections,
    latencyNeeded,
    checkpoints,
    idleLatency: env.idleLatency,
    now,
  };
  const connect = (entry: CatalogEntry) => {
    const held = grants.get(entry.id);
    const auth = held?.url === entry.url ? held.auth : initialAuth(entry, page, env.session);
    const isSelected = computed(() => selected().includes(entry.id));
    return createConnection(entry, auth, isSelected, connectionEnv);
  };

  /** Rewrites the listed roles' choices to apply to every server, as the first server selected so far reads them. */
  function travel(roles: readonly Role[]): void {
    const lead = selected.peek()[0];
    const discovery = (lead !== undefined && views.peek().get(lead)?.discovery) || null;
    const { transports } = config.peek();
    const next = { ...transports };
    for (const role of roles)
      next[TRANSPORT_KEY[role]] = selectionText(portable(role, selections[role].peek(), discovery, page));
    if (next.throughputTarget !== transports.throughputTarget || next.latencyTarget !== transports.latencyTarget)
      env.setTransports(next);
  }

  function install(next: Catalog): void {
    const { ids, unresolved: rows } = reconcile(next, readSaved());
    const chosen = next.servers.filter((entry) => ids.includes(entry.id));
    // Exact origins can be chosen only with a single server, and only one it allows.
    const travelling = ROLES.filter((role) => {
      const selection = selections[role].peek();
      const origin = selection.kind === "origin" ? selection.origin : null;
      return chosen.length > 1 || (origin !== null && chosen.some((entry) => !allowsOrigin(entry, origin)));
    });
    batch(() => {
      travel(travelling);
      for (const connection of connections.peek().values()) connection.dispose();
      for (const [id, held] of grants)
        if (next.servers.find((entry) => entry.id === id)?.url !== held.url) grants.delete(id);
      catalogue.set(next);
      selected.set(ids);
      unresolved.set(rows);
      loadError.set("");
      connections.set(new Map(next.servers.map((entry) => [entry.id, connect(entry)])));
    });
    limiter.drain();
  }

  async function load(): Promise<void> {
    loadRequest?.abort();
    const request = new AbortController();
    loadRequest = request;
    batch(() => {
      metadata.set(false);
      cancelSignIn();
      loading.set(true);
    });
    try {
      const signal = AbortSignal.any([request.signal, lifetime.signal, AbortSignal.timeout(CATALOGUE_TIMEOUT_MS)]);
      const response = await env.session.fetch(ROUTES.servers, { cache: "no-store", signal });
      if (!response.ok) {
        response.body?.cancel().catch(() => {});
        throw new Error(`the catalogue answered HTTP ${response.status}`);
      }
      const next = catalogDocument(await readJSON(response), page.origin);
      signal.throwIfAborted();
      install(next);
    } catch {
      if (loadRequest === request && !lifetime.signal.aborted) loadError.set("Could not load the server catalog");
    } finally {
      if (loadRequest === request) {
        loadRequest = null;
        loading.set(false);
      }
    }
  }

  async function retry(id?: string, role?: Role): Promise<void> {
    if (!env.active.peek()) return;
    const chosen = selected.peek().filter((each) => !id || each === id);
    await Promise.all(chosen.map((each) => connections.peek().get(each)?.check({ force: true, role })));
  }

  function apply(ids: readonly string[]): boolean {
    const known = catalogue.peek();
    if (!known || loading.peek() || !env.active.peek()) return false;
    if (
      ids.length < 1 ||
      ids.length > MAX_SELECTED ||
      new Set(ids).size !== ids.length ||
      ids.some((id) => !known.servers.some((entry) => entry.id === id))
    )
      throw new Error("Select one to four catalog servers");
    const next = inCatalogueOrder(known, ids);
    const previous = selected.peek();
    batch(() => {
      if (next.length !== previous.length || next.some((id, i) => id !== previous[i])) travel(ROLES);
      const current = approval.peek();
      if (current.state !== "idle" && !next.includes(current.id)) cancelSignIn();
      selected.set(next);
      unresolved.set([]);
    });
    limiter.drain();
    writeSaved(known.servers.filter((entry) => next.includes(entry.id)).map(({ id, url }) => ({ id, url })));
    return true;
  }

  function cancelSignIn(): void {
    flow?.abort();
    flow = null;
    approval.set(IDLE);
  }

  function signIn(id: string): void {
    const entry = catalogue.peek()?.servers.find((each) => each.id === id);
    if (!entry || !selected.peek().includes(id) || loading.peek() || !env.active.peek()) return;
    cancelSignIn();
    const abort = new AbortController();
    flow = abort;
    const popup = reserveWindow(abort.signal);
    approval.set({ state: "preparing", id });
    void approve(entry, abort, popup);
  }

  async function approve(entry: CatalogEntry, abort: AbortController, popup: ReturnType<typeof reserveWindow>) {
    const { signal } = abort;
    let shown: { url: string; code: string } | null = null;
    try {
      const { url, code, verifier } = await beginApproval(entry, page);
      signal.throwIfAborted();
      shown = { url, code };
      approval.set({ state: "waiting", id: entry.id, url, code });
      popup.navigate(url);
      const grant = await exchangeGrant(entry, verifier, signal, now);
      if (flow !== abort || catalogue.peek()?.servers.find((each) => each.id === entry.id)?.url !== entry.url) return;
      grants.set(entry.id, { url: entry.url, grant, auth: grantAuth(entry, grant, now) });
      // Approval owns the grant exchange; the new connection owns every later path error.
      batch(() => {
        const old = connections.peek();
        old.get(entry.id)?.dispose();
        connections.set(new Map([...old, [entry.id, connect(entry)]]));
        approval.set(IDLE);
      });
    } catch (cause) {
      if (signal.aborted) return;
      connections
        .peek()
        .get(entry.id)
        ?.requireSignIn(cause instanceof Error ? cause.message : "Approval failed");
      if (cause instanceof ApprovalLimitError)
        approval.set({ state: "limit", id: entry.id, renewUrl: `${entry.url}/login` });
      else approval.set(shown ? { state: "failed", id: entry.id, ...shown } : IDLE);
    } finally {
      popup.close();
      if (flow === abort) flow = null;
    }
  }

  const chosenConnections = () => selected.peek().flatMap((id) => connections.peek().get(id) ?? []);

  async function authorize(requiredMs: number, signal: AbortSignal): Promise<void> {
    budget = null;
    let earliest = await env.session.coverage(requiredMs, signal);
    for (const connection of chosenConnections()) {
      const held = grants.get(connection.entry.id);
      if (!held || connection.auth !== held.auth) continue;
      const { name } = connection.view.peek().server;
      if (requiredMs > held.grant.maximumLifetimeMs)
        throw new CoverageError(`${name}: this test is longer than its sign-in can last. Shorten it before starting.`);
      if (held.grant.deadline - now() < requiredMs) {
        connection.requireSignIn("Sign in again to cover the planned test duration");
        throw new CoverageError(`${name}: sign in again to cover the planned test duration`);
      }
      earliest = Math.min(earliest ?? Infinity, held.grant.deadline);
    }
    budget = earliest;
  }

  async function prepare(signal: AbortSignal) {
    const chosen = chosenConnections();
    // One judgement of freshness: what the checks keep is what the run may use, however long they take.
    const since = now() - FRESH_MS;
    await Promise.all(chosen.map((connection) => connection.check({ since, signal })));
    signal.throwIfAborted();
    if (chosen.some((connection) => connections.peek().get(connection.entry.id) !== connection))
      throw new DOMException("Connection selection changed", "AbortError");
    const servers: PreparedServer[] = [];
    const dropped: DroppedServer[] = [];
    for (const connection of chosen) {
      const ready = connection.prepared(since);
      if (ready) {
        servers.push(ready);
        continue;
      }
      const view = connection.view.peek();
      const reason = view.readiness === "sign-in" ? "sign-in-required" : "preparation-failed";
      // Evidence lost while the checks ran, as to a network change, leaves no message of its own.
      dropped.push({ server: view.server, reason, message: view.message ?? "Connection check failed" });
    }
    const shown = servers.find(({ server }) => server.id === latencyServer.peek()) ?? servers[0];
    return { servers, dropped, focus: shown?.server.id ?? "" };
  }

  return {
    catalogue,
    loading,
    loadError,
    selected,
    unresolved,
    views,
    approval,
    selectionState: computed(() => {
      if (loading()) return "checking";
      if (unresolved().length || !selected().length) return "failed";
      const states = selected().map((id) => views().get(id)?.readiness ?? "unchecked");
      if (states.includes("checking")) return "checking";
      if (states.some((state) => state === "failed" || state === "sign-in")) return "failed";
      return states.every((state) => state === "verified") ? "verified" : "unchecked";
    }),
    blocker: computed(() => {
      if (!catalogue()) return loading() ? "" : "Open Settings to retry loading the server list.";
      if (unresolved().length || !selected().length)
        return "The saved selection changed. Open Settings to choose the servers to test.";
      const blocked = selectedViews().find((view) => view.blocked);
      if (!blocked?.blocked) return "";
      return selected().length > 1 ? `${blocked.server.name}: ${blocked.blocked}` : blocked.blocked;
    }),
    stageLimit: computed(() => {
      let limit = { ms: MAX_STAGE_LIMIT_MS, server: null as string | null };
      for (const view of selectedViews())
        if (view.discovery && view.discovery.maxStageMs < limit.ms)
          limit = { ms: view.discovery.maxStageMs, server: view.server.name };
      return limit;
    }),
    streamPlanError: computed(() => {
      const servers = selectedViews().map((view) => (view.ready ? planServer(view) : null));
      return servers.every((server) => server !== null) ? planError(config(), servers) : "";
    }),
    streams: computed(() => {
      const id = representative();
      const view = id === null ? undefined : views().get(id);
      return describeStreams(config().transferStreams, activities(), view ? planServer(view) : null);
    }),
    latencyServer,
    metadataLoading: computed(() => [...views().values()].some((view) => view.metadataChecking)),
    load,
    async retryCatalogue() {
      if (!env.active.peek()) return;
      await load();
      if (!loadError.peek()) await retry();
    },
    apply,
    signIn,
    cancelSignIn,
    retry,
    showMetadata(open) {
      if (!open || (catalogue.peek() && env.active.peek())) metadata.set(open);
    },
    authorize,
    prepare,
    fits: (totalMs) => budget === null || now() + totalMs <= budget,
    recheck(failure) {
      if (!failure) {
        for (const connection of chosenConnections()) connection.invalidate(ROLES);
        return;
      }
      const connection = connections.peek().get(failure.serverId);
      if (failure.reason === "sign-in-required") connection?.requireSignIn(failure.message || undefined);
      else connection?.invalidate([failure.scope]);
    },
    resume() {
      for (const connection of connections.peek().values()) connection.resume();
    },
    dispose() {
      lifetime.abort();
      loadRequest?.abort();
      cancelSignIn();
      batch(() => {
        for (const connection of connections.peek().values()) connection.dispose();
        connections.set(new Map());
      });
    },
  };
}
