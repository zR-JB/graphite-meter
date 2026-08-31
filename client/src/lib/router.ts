import { isUuid } from "./uuid";

export type PanelSurface = "settings" | "endpoint";
export type DialogSurface = "legal";

export type Workspace =
  { kind: "measurement" } | { kind: "history"; selectedId: string | null };

export type AppRoute = {
  kind: "app";
  workspace: Workspace;
  panels: PanelSurface[];
  dialog: DialogSurface | null;
};

export type Route = AppRoute | { kind: "not-found" };

const PANELS: readonly PanelSurface[] = ["settings", "endpoint"];
const MAX_HASH_LENGTH = 4_096;

function uniquePanels(panels: readonly PanelSurface[]): PanelSurface[] {
  return [...new Set(panels.filter((panel) => PANELS.includes(panel)))];
}

export function appRoute(
  workspace: Workspace = { kind: "measurement" },
  panels: readonly PanelSurface[] = [],
  dialog: DialogSurface | null = null,
): AppRoute {
  return { kind: "app", workspace, panels: uniquePanels(panels), dialog };
}

function surfaces(query: string): Pick<AppRoute, "panels" | "dialog"> {
  const params = new URLSearchParams(query);
  const panels =
    params
      .get("panels")
      ?.split(",")
      .filter((panel): panel is PanelSurface =>
        PANELS.includes(panel as PanelSurface),
      ) ?? [];
  return {
    panels: uniquePanels(panels),
    dialog: params.get("dialog") === "legal" ? "legal" : null,
  };
}

export function parseRoute(hash: string): Route {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  if (raw.length > MAX_HASH_LENGTH) return { kind: "not-found" };
  const [path, query = ""] = raw.split("?", 2);
  const overlay = surfaces(query);

  if (path === "" || path === "/")
    return appRoute({ kind: "measurement" }, overlay.panels, overlay.dialog);
  if (path === "/history")
    return appRoute(
      { kind: "history", selectedId: null },
      overlay.panels,
      overlay.dialog,
    );
  if (path.startsWith("/history/")) {
    let id: string;
    try {
      id = decodeURIComponent(path.slice("/history/".length));
    } catch {
      return { kind: "not-found" };
    }
    if (!isUuid(id)) return { kind: "not-found" };
    return appRoute(
      { kind: "history", selectedId: id },
      overlay.panels,
      overlay.dialog,
    );
  }

  // Friendly routes cover one measurement surface; composites use the query.
  if (path === "/settings")
    return appRoute(
      { kind: "measurement" },
      ["settings", ...overlay.panels],
      overlay.dialog,
    );
  if (path === "/endpoint")
    return appRoute(
      { kind: "measurement" },
      ["endpoint", ...overlay.panels],
      overlay.dialog,
    );
  if (path === "/legal")
    return appRoute({ kind: "measurement" }, overlay.panels, "legal");
  return { kind: "not-found" };
}

export function serializeRoute(route: Route): string {
  if (route.kind === "not-found") return "#/not-found";
  const panels = uniquePanels(route.panels);
  const { workspace, dialog } = route;

  if (workspace.kind === "measurement") {
    if (!dialog && panels.length === 0) return "#/";
    if (!dialog && panels.length === 1) return `#/${panels[0]}`;
    if (dialog === "legal" && panels.length === 0) return "#/legal";
  }

  const path =
    workspace.kind === "measurement"
      ? "#/"
      : workspace.selectedId
        ? `#/history/${encodeURIComponent(workspace.selectedId)}`
        : "#/history";
  // Closed enums keep composed routes readable without encoded separators.
  const query = [
    panels.length ? `panels=${panels.join(",")}` : "",
    dialog ? `dialog=${dialog}` : "",
  ]
    .filter(Boolean)
    .join("&");
  return query ? `${path}?${query}` : path;
}

export function withWorkspace(route: Route, workspace: Workspace): AppRoute {
  return route.kind === "app"
    ? appRoute(workspace, route.panels, route.dialog)
    : appRoute(workspace);
}

export function activatePanel(
  route: Route,
  panel: PanelSurface,
  allowMultiple = true,
): AppRoute {
  const base = route.kind === "app" ? route : appRoute();
  return appRoute(
    base.workspace,
    allowMultiple
      ? [...base.panels.filter((candidate) => candidate !== panel), panel]
      : [panel],
    base.dialog,
  );
}

export function reconcilePanels(route: Route, allowMultiple: boolean): Route {
  if (route.kind !== "app" || allowMultiple || route.panels.length <= 1)
    return route;
  return appRoute(route.workspace, [route.panels.at(-1)!], route.dialog);
}

export function closePanel(route: Route, panel: PanelSurface): AppRoute {
  const base = route.kind === "app" ? route : appRoute();
  return appRoute(
    base.workspace,
    base.panels.filter((candidate) => candidate !== panel),
    base.dialog,
  );
}

export function openDialog(route: Route, dialog: DialogSurface): AppRoute {
  const base = route.kind === "app" ? route : appRoute();
  return appRoute(base.workspace, base.panels, dialog);
}

export function closeDialog(route: Route): AppRoute {
  const base = route.kind === "app" ? route : appRoute();
  return appRoute(base.workspace, base.panels, null);
}
