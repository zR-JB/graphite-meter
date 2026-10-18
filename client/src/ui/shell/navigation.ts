// Hash routes: a surface the app opened closes by stepping back, one reached by a link by replacing.

export type Panel = "settings" | "details";
export type Workspace = { kind: "measurement" } | { kind: "history"; id: string | null };
export type Route =
  | { kind: "app"; workspace: Workspace; panels: readonly Panel[]; legal: boolean }
  | { kind: "not-found" };

const MEASUREMENT: Workspace = { kind: "measurement" };
const MAX_HASH = 4_096;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const PANEL_WORD: Record<Panel, string> = { settings: "settings", details: "endpoint" };
const PANEL_OF = new Map<string, Panel>([
  ["settings", "settings"],
  ["endpoint", "details"],
]);

export const appRoute = (
  workspace: Workspace = MEASUREMENT,
  panels: readonly Panel[] = [],
  legal = false,
): Route => ({
  kind: "app",
  workspace,
  panels: [...new Set(panels)],
  legal,
});

export function parseRoute(hash: string): Route {
  const text = hash.startsWith("#") ? hash.slice(1) : hash;
  if (text.length > MAX_HASH) return { kind: "not-found" };
  const [path = "", query = ""] = text.split("?", 2);
  const params = new URLSearchParams(query);
  const panels = (params.get("panels") ?? "")
    .split(",")
    .flatMap((word) => PANEL_OF.get(word) ?? []);
  const legal = params.get("dialog") === "legal";
  if (path === "" || path === "/") return appRoute(MEASUREMENT, panels, legal);
  if (path === "/history") return appRoute({ kind: "history", id: null }, panels, legal);
  if (path.startsWith("/history/")) {
    let id: string;
    try {
      id = decodeURIComponent(path.slice("/history/".length));
    } catch {
      return { kind: "not-found" };
    }
    return UUID.test(id) ? appRoute({ kind: "history", id }, panels, legal) : { kind: "not-found" };
  }
  const panel = PANEL_OF.get(path.slice(1));
  if (panel) return appRoute(MEASUREMENT, [panel, ...panels], legal);
  if (path === "/legal") return appRoute(MEASUREMENT, panels, true);
  return { kind: "not-found" };
}

/** The canonical hash: the shortest path form a single surface has, else a path with a query. */
export function serializeRoute(route: Route): string {
  if (route.kind === "not-found") return "#/not-found";
  const { workspace, panels, legal } = route;
  if (workspace.kind === "measurement") {
    if (!legal && !panels.length) return "#/";
    if (!legal && panels.length === 1) return `#/${PANEL_WORD[panels[0]!]}`;
    if (legal && !panels.length) return "#/legal";
  }
  const path =
    workspace.kind === "measurement"
      ? "#/"
      : workspace.id
        ? `#/history/${encodeURIComponent(workspace.id)}`
        : "#/history";
  // Closed vocabularies keep composed routes readable, so the commas stay unencoded.
  const query = [
    panels.length && `panels=${panels.map((panel) => PANEL_WORD[panel]).join(",")}`,
    legal && "dialog=legal",
  ].filter(Boolean);
  return query.length ? `${path}?${query.join("&")}` : path;
}

const app = (route: Route) =>
  route.kind === "app" ? route : (appRoute() as Extract<Route, { kind: "app" }>);
export const withWorkspace = (route: Route, workspace: Workspace) =>
  appRoute(workspace, app(route).panels, app(route).legal);
/** The panel moves to the end: the end is the last opened. */
export const openPanel = (route: Route, panel: Panel) =>
  appRoute(
    app(route).workspace,
    [...app(route).panels.filter((each) => each !== panel), panel],
    app(route).legal,
  );
export const closePanel = (route: Route, panel: Panel) =>
  appRoute(
    app(route).workspace,
    app(route).panels.filter((each) => each !== panel),
    app(route).legal,
  );
export const withLegal = (route: Route, legal: boolean) =>
  appRoute(app(route).workspace, app(route).panels, legal);

/** Where only one panel fits, a route keeps the last one opened. */
export const fitPanels = (route: Route, multiple: boolean): Route =>
  route.kind === "app" && !multiple && route.panels.length > 1
    ? appRoute(route.workspace, route.panels.slice(-1), route.legal)
    : route;

interface Marker {
  graphiteRoute: boolean;
  /** The hash this entry was pushed from. */
  parent?: string;
}

interface Navigator {
  /** Opens `next` on a new history entry. */
  push(next: Route): void;
  /** Rewrites the current entry, keeping where it was pushed from. */
  replace(next: Route): void;
  /** Closes a surface: back to the entry it was opened from, else a replace. */
  close(next: Route): void;
  /** Applies the location the page loaded with; `refit` re-applies the panel rule when the layout changes. */
  start(): void;
  refit(): void;
  dispose(): void;
}

/** Owns the URL. `commit` renders a route in the frame of the input that asked for it; `fromHistory` marks the
    browser's own navigations (Back, Forward, links, the first load). */
export function createNavigator(env: {
  location: Pick<Location, "hash">;
  history: Pick<History, "state" | "pushState" | "replaceState" | "back">;
  target: Pick<Window, "addEventListener" | "removeEventListener">;
  multiple: () => boolean;
  current: () => Route;
  commit: (next: Route, fromHistory: boolean) => void;
}): Navigator {
  const { location, history } = env;
  let traversing = false;
  let queued: { route: Route; replace: boolean } | null = null;
  const marker = (): Marker => {
    const state = history.state as Marker | null;
    return typeof state === "object" && state !== null && typeof state.graphiteRoute === "boolean"
      ? state
      : { graphiteRoute: false };
  };
  const fit = (route: Route) => fitPanels(route, env.multiple());

  // The parent is the entry being left: the URL still names it while its route already rendered.
  const write = (route: Route, replace: boolean) => {
    const hash = serializeRoute(route);
    if (replace) history.replaceState(marker(), "", hash);
    else if (location.hash !== hash)
      history.pushState({ graphiteRoute: true, parent: location.hash }, "", hash);
  };
  const go = (next: Route, replace: boolean) => {
    const route = fit(next);
    env.commit(route, false);
    if (traversing) queued = { route, replace };
    else write(route, replace);
  };
  const arrive = () => {
    const route = fit(parseRoute(location.hash));
    const hash = serializeRoute(route);
    if (location.hash !== hash) history.replaceState(marker(), "", hash);
    env.commit(route, true);
  };
  const popstate = () => {
    if (!traversing) return arrive();
    traversing = false;
    if (queued) write(queued.route, queued.replace);
    queued = null;
  };
  env.target.addEventListener("popstate", popstate);

  return {
    push: (next) => go(next, false),
    replace: (next) => go(next, true),
    close(next) {
      const route = fit(next);
      const { graphiteRoute, parent } = marker();
      if (traversing || !graphiteRoute || parent !== serializeRoute(route)) return go(route, true);
      traversing = true;
      env.commit(route, true);
      history.back();
    },
    start: arrive,
    refit() {
      const route = env.current();
      if (fit(route) !== route) go(route, true);
    },
    dispose: () => env.target.removeEventListener("popstate", popstate),
  };
}
