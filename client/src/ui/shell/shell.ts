// The shell's state and moves: route, panels, focus, keys, theme and starting over; the console only renders it.
import type { App } from "../../app/app";
import type { Theme } from "../../app/preferences";
import { reducedMotion } from "../../lib/motion";
import { batch, computed, effect, onCleanup, signal, untrack, type Signal } from "../../lib/signal";
import { runAction } from "../../view/words";
import { flip } from "../flip";
import { activeModal, canFocus, hasFocus, isEditable } from "../kit/focus";
import type { Shell } from "./api";
import { command, type Command, type KeyFacts } from "./keys";
import { DOCK, resolveDocks, type Layout, type Mode, type Side } from "./layout";
import {
  closePanel,
  createNavigator,
  fitPanels,
  openPanel,
  parseRoute,
  serializeRoute,
  withLegal,
  withWorkspace,
  type Panel,
  type Route,
} from "./navigation";

const PANELS = ["settings", "details"] as const satisfies readonly Panel[];
export const SIDE: Record<Panel, Side> = { settings: "left", details: "right" };
const MEASUREMENT = { kind: "measurement" } as const;

/** Where focus goes once a route has rendered: an element, the workspace it shows, or nowhere new. */
type Focus = HTMLElement | "workspace" | "keep";
type Workspace = KeyFacts["workspace"];

// A focused control that acts on Space keeps it.
const OWNS_SPACE =
  "button, a[href], summary, input, [role=button], [role=link], [role=switch], [role=checkbox], [role=radio], " +
  "[role=tab], [role=menuitem], [role=option], [role=slider], [role=separator]";
const KEYS = new Set(["escape", " ", "r", "s", "d", "h", "t"]);
// Keys that only arm or modify a chord; any other key shows where focus is.
const MODIFIER = /^(Shift|Control|Alt|AltGraph|Meta|OS|CapsLock|Fn|PrintScreen)$/;

const workspaceOf = (route: Route): Workspace =>
  route.kind === "app" ? route.workspace.kind : "not-found";
const legalOf = (route: Route) => route.kind === "app" && route.legal;
const resultOf = (route: Route) =>
  route.kind === "app" && route.workspace.kind === "history" ? (route.workspace.id ?? "") : null;

/** What a route change moves on the spring: the stage and sheets as one surface, or the History panes; null when it applies in place. */
export function moves(
  current: Route,
  next: Route,
  docked: boolean,
  multiple: boolean,
): readonly string[] | null {
  const docks = (route: Route) => {
    const fit = fitPanels(route, multiple);
    return docked && fit.kind === "app" ? fit.panels.toSorted().join() : "";
  };
  if (docks(next) !== docks(current)) return ["stage", "sheet-left", "sheet-right"];
  const [from, to] = [resultOf(current), resultOf(next)];
  return from !== null && to !== null && from !== to ? ["history-list", "history-detail"] : null;
}

type FocusMove = { to: "title" | "invoker"; panel: Panel } | { to: "wanted" | "workspace" } | null;

/** Where focus goes once a route has rendered, or null to leave it. */
export function focusMove(facts: {
  before: Route;
  after: Route;
  /** The panels visible now; the route before names those that were open, whatever the layout showed. */
  shown: readonly Panel[];
  /** The panel that held focus; asked only when a panel closed, as asking lays out the page. */
  holder: Panel | null;
  focused(): boolean;
  wanted: "element" | "workspace" | "keep" | null;
  fromHistory: boolean;
  flyout: boolean;
}): FocusMove {
  const { before, after, shown, holder } = facts;
  const named = before.kind === "app" ? before.panels : [];
  const opened = shown.findLast((panel) => !named.includes(panel));
  if (opened) return { to: "title", panel: opened };
  if ((legalOf(after) && !legalOf(before)) || facts.wanted === "keep") return null;
  if (facts.wanted) return { to: facts.wanted === "element" ? "wanted" : "workspace" };
  const closed = named.filter((panel) => !shown.includes(panel));
  if (holder && closed.includes(holder)) return { to: "invoker", panel: holder };
  if (closed.length) return facts.focused() ? null : { to: "invoker", panel: closed.at(-1)! };
  const switched = facts.fromHistory && workspaceOf(before) !== workspaceOf(after);
  return switched && !legalOf(after) && !facts.flyout ? { to: "workspace" } : null;
}

export type Controller = ReturnType<typeof createShell>;

const NEXT_THEME: Record<Theme, Theme> = { light: "dark", dark: "auto", auto: "light" };
/** How long a start may prepare before the run key says Cancel rather than Stop. */
const CANCEL_AFTER_MS = 250;

/** The shell for one console. Must run inside an owner: its listeners and effects go with it. */
export function createShell(app: App, layout: Layout) {
  const multiple = () => layout.mode.peek() === "docked2";
  // Parsed at once, so the panels a deep link names are built with the console.
  const route = signal<Route>(fitPanels(parseRoute(location.hash), multiple()));
  const initial = route.peek();
  const visited = Object.fromEntries(
    PANELS.map((panel) => [
      panel,
      signal(initial.kind === "app" && initial.panels.includes(panel)),
    ]),
  ) as Record<Panel, Signal<boolean>>;

  const workspace = computed(() => workspaceOf(route()));
  /** The visible panels, the last opened last: one where only one fits. */
  const visible = (current: Route, mode: Mode): readonly Panel[] =>
    current.kind !== "app" ? [] : mode === "docked2" ? current.panels : current.panels.slice(-1);
  const panels = computed(() => visible(route(), layout.mode()));
  const shown = (panel: Panel) => panels().includes(panel);
  const docked = computed(() => layout.mode() !== "flyout");
  /** A panel flies over the stage, which waits inert beneath it. */
  const flyout = computed(() => !docked() && panels().length > 0);
  const legalOpen = computed(() => legalOf(route()));
  const historyOpen = computed(() => workspace() === "history");
  const selectedId = computed(() => resultOf(route()) || null);

  // The side resized last keeps its width when both must shrink, so a dragged edge follows the pointer.
  const resized = signal<Side | null>(null);
  const docks = computed(() =>
    resolveDocks(
      layout.width(),
      app.preferences.dockWidth(),
      { left: docked() && shown("settings"), right: docked() && shown("details") },
      resized(),
    ),
  );
  const resize = (side: Side, px: number) =>
    batch(() => {
      resized.set(side);
      app.preferences.set({ dockWidth: { ...app.preferences.dockWidth.peek(), [side]: px } });
    });
  const resetWidth = (side: Side) => resize(side, DOCK.preferred);

  /** What renders each surface, for focus: the sheets, and the workspaces' own focus targets. */
  const elements: {
    sheets: Partial<Record<Panel, HTMLElement>>;
    measurement: HTMLElement | null;
    history: HTMLElement | null;
  } = { sheets: {}, measurement: null, history: null };

  /** Who opened each surface: focus goes back there when it closes, if it still can take it. */
  const invokers = new Map<Panel | "history", HTMLElement>();
  let legalInvoker: HTMLElement | null = null;
  let intent: Focus | undefined;

  const holding = (target: EventTarget | null): Panel | null =>
    target instanceof Node
      ? (PANELS.find((panel) => elements.sheets[panel]?.contains(target)) ?? null)
      : null;
  const workspaceTarget = (): HTMLElement | null =>
    workspace.peek() === "history"
      ? (elements.history?.querySelector<HTMLElement>(".history-workspace") ?? null)
      : workspace.peek() === "measurement"
        ? elements.measurement
        : null;
  const focus = (target: HTMLElement | null | undefined) => {
    const to = canFocus(target) ? target : workspaceTarget();
    to?.focus({ preventScroll: true });
  };

  const title = (panel: Panel) =>
    elements.sheets[panel]?.querySelector<HTMLElement>(".sheet-head h2");

  function apply(next: Route, fromHistory: boolean, wanted: Focus | undefined): void {
    const before = route.peek();
    const holder = holding(document.activeElement);
    batch(() => {
      route.set(next);
      if (next.kind === "app") for (const panel of next.panels) visited[panel].set(true);
    });
    const shown = panels.peek();
    const move = focusMove({
      before,
      after: next,
      shown,
      holder,
      focused: hasFocus,
      wanted: wanted instanceof HTMLElement ? "element" : (wanted ?? null),
      fromHistory,
      flyout: flyout.peek(),
    });
    const invoker = move?.to === "invoker" ? invokers.get(move.panel) : undefined;
    if (before.kind === "app")
      for (const panel of before.panels) if (!shown.includes(panel)) invokers.delete(panel);
    if (move?.to === "title") title(move.panel)?.focus({ preventScroll: true });
    else if (move)
      focus(
        move.to === "invoker" ? invoker : move.to === "wanted" ? (wanted as HTMLElement) : null,
      );
  }

  function commit(next: Route, fromHistory: boolean): void {
    const wanted = intent;
    intent = undefined;
    const update = () => apply(next, fromHistory, wanted);
    const keys = moves(route.peek(), next, docked.peek(), multiple());
    if (!keys) return update();
    flip(update, keys);
  }

  const navigator = createNavigator({
    location,
    history,
    target: window,
    multiple,
    current: () => route.peek(),
    commit,
  });
  onCleanup(navigator.dispose);
  // Narrowed below two docks, the first-opened panel closes and the URL follows without a new entry.
  effect(() => {
    layout.mode();
    untrack(navigator.refit);
  });
  const navigate = (next: Route, how: "push" | "replace" | "close", focusAfter?: Focus) => {
    if (serializeRoute(fitPanels(next, multiple())) === serializeRoute(route.peek())) return;
    intent = focusAfter;
    try {
      navigator[how](next);
    } finally {
      intent = undefined;
    }
  };

  function open(panel: Panel, invoker?: HTMLElement | null): void {
    const last = panels.peek().at(-1);
    // Where one panel fits, another replaces it on the same entry, which still steps back to where it opened.
    const replacing = !multiple() && last !== undefined && last !== panel;
    if (invoker) invokers.set(panel, invoker);
    else invokers.delete(panel);
    if (replacing) invokers.delete(last);
    navigate(openPanel(route.peek(), panel), replacing ? "replace" : "push");
  }
  function close(panel: Panel, invoker?: HTMLElement | null): void {
    if (invoker) invokers.set(panel, invoker);
    navigate(closePanel(route.peek(), panel), "close");
  }
  const toggle = (panel: Panel, invoker?: HTMLElement | null) =>
    untrack(() => (shown(panel) ? close(panel, invoker) : open(panel, invoker)));

  function showHistory(id: string | null, invoker?: HTMLElement | null, focusAfter?: Focus): void {
    const current = route.peek();
    // Back from a result to the list is a close: it steps back to where the result opened.
    if (id === null && resultOf(current))
      return navigate(withWorkspace(current, { kind: "history", id }), "close");
    if (id === null && !historyOpen.peek()) {
      if (invoker) invokers.set("history", invoker);
      else invokers.delete("history");
    }
    navigate(withWorkspace(current, { kind: "history", id }), "push", focusAfter);
  }
  /** Stepping through results rewrites the entry, so Back and closing still return to where the first opened. */
  function swapResult(id: string): void {
    if (resultOf(route.peek()))
      navigate(withWorkspace(route.peek(), { kind: "history", id }), "replace");
  }
  function closeHistory(focusAfter?: Focus): void {
    const invoker = invokers.get("history");
    invokers.delete("history");
    navigate(
      withWorkspace(route.peek(), MEASUREMENT),
      "close",
      focusAfter ?? (canFocus(invoker) ? invoker : "workspace"),
    );
  }
  /** A key or menu item keeps focus on itself through both ways. */
  const toggleHistory = (key: HTMLElement) =>
    historyOpen.peek() ? closeHistory(key) : showHistory(null, key, key);
  /** The shortcut moves focus to the workspace it shows, unless a flyout holds it. */
  function historyShortcut(): void {
    const where: Focus = flyout.peek() ? "keep" : "workspace";
    if (historyOpen.peek()) closeHistory(where);
    else showHistory(null, null, where);
  }

  // Starting over asks first while a run is live or its result unsaved, and a new run first when the result it replaces failed to save.
  const reset = { open: signal(false), reason: signal<"running" | "unsaved" | "rerun">("running") };
  const unsavable = () => !!app.run.result.peek() && app.saver.failed.peek();
  function startOver(): void {
    if (workspace.peek() !== "measurement")
      return navigate(withWorkspace(route.peek(), MEASUREMENT), "push");
    const running = app.run.running.peek();
    if (!running && !(app.run.result.peek() && app.saver.unsaved.peek()))
      return app.run.returnToStart();
    batch(() => {
      reset.reason.set(running ? "running" : "unsaved");
      reset.open.set(true);
    });
  }
  function confirmStartOver(): void {
    reset.open.set(false);
    if (reset.reason.peek() === "rerun") app.run.toggle();
    else app.run.returnToStart();
  }
  function toggleRun(): void {
    const starting = !app.run.running.peek() && !app.run.preparing.peek();
    if (!starting || !unsavable()) return app.run.toggle();
    batch(() => {
      reset.reason.set("rerun");
      reset.open.set(true);
    });
  }

  /** Light, dark, auto: each press steps from the theme applied when it runs, fading in on the compositor; a skipped fade is no error. */
  function toggleTheme(): void {
    const update = () => app.preferences.set({ theme: NEXT_THEME[app.preferences.theme.peek()] });
    if (reducedMotion.peek() || !document.startViewTransition) return update();
    try {
      document.startViewTransition({ update, types: ["theme"] }).ready.catch(() => {});
    } catch {
      // An engine that takes only a callback refuses the options before it runs the update.
      update();
    }
  }

  const returnToLive = () =>
    navigate(withWorkspace(route.peek(), MEASUREMENT), "push", "workspace");

  const legal = (invoker?: HTMLElement | null) => {
    legalInvoker = invoker ?? null;
    navigate(withLegal(route.peek(), true), "push");
  };
  const closeLegal = () => navigate(withLegal(route.peek(), false), "close");

  function run(order: Command): void {
    switch (order.kind) {
      case "close":
        return close(order.panel);
      case "close-detail":
        return showHistory(null);
      case "close-history":
        return closeHistory();
      case "cancel-start":
        return app.run.cancel();
      case "stop":
        return app.run.toggle();
      case "run":
        return toggleRun();
      case "toggle":
        return toggle(order.panel);
      case "history":
        return historyShortcut();
      case "theme":
        return toggleTheme();
    }
  }

  // One page-level handler, bubbling, so local widgets, menus, modals and tips take their keys first.
  function keydown(event: KeyboardEvent): void {
    if (event.defaultPrevented) return;
    // Escape dismisses whatever is held with it; every other key acts only on its own.
    if (
      event.key !== "Escape" &&
      (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey)
    )
      return;
    if (!KEYS.has(event.key.toLowerCase())) return;
    if (document.querySelector(":popover-open:not(.tooltip)") || activeModal()) return;
    const target = event.target;
    const current = route.peek();
    const order = command({
      key: event.key,
      editable: isEditable(target),
      ownsSpace: target instanceof Element && target.matches(OWNS_SPACE),
      shortcuts: app.preferences.keyShortcuts.peek(),
      saving: app.saving.peek(),
      workspace: workspace.peek(),
      detail: !!resultOf(current),
      panels: panels.peek(),
      focused: holding(target),
      flyout: flyout.peek(),
      pending: app.run.preparing.peek(),
      running: app.run.running.peek(),
    });
    if (!order) return;
    event.preventDefault();
    // Every shortcut toggles or acts once: a held key acts on its press, and its repeats do nothing.
    if (!event.repeat) run(order);
  }
  // Leaving mid-run, before a result's write lands, or with an unsaved result asks first; a sign-out asked already.
  const beforeunload = (event: BeforeUnloadEvent) => {
    if (app.signingOut.peek()) return;
    if (!app.run.running.peek() && !app.saver.pending.peek() && !unsavable()) return;
    event.preventDefault();
    event.returnValue = "";
  };
  // Chromium lights a clicked control's ring on any later key; here only a key that moves focus does.
  const root = document.documentElement;
  const pointer = () => {
    root.dataset.pointer = "";
  };
  const typed = (event: KeyboardEvent) => {
    if (!event.ctrlKey && !event.metaKey && !event.altKey && !MODIFIER.test(event.key))
      delete root.dataset.pointer;
  };
  window.addEventListener("keydown", keydown);
  window.addEventListener("beforeunload", beforeunload);
  window.addEventListener("pointerdown", pointer, true);
  window.addEventListener("keydown", typed, true);
  onCleanup(() => {
    window.removeEventListener("keydown", keydown);
    window.removeEventListener("beforeunload", beforeunload);
    window.removeEventListener("pointerdown", pointer, true);
    window.removeEventListener("keydown", typed, true);
    delete root.dataset.pointer;
  });

  // A start that is still preparing after a glance offers Cancel; a quick one goes straight to Stop.
  const lingering = signal(false);
  effect(() => {
    if (!app.run.preparing()) return lingering.set(false);
    const timer = setTimeout(() => lingering.set(true), CANCEL_AFTER_MS);
    onCleanup(() => clearTimeout(timer));
  });

  const shell: Shell = {
    route,
    layout,
    shown,
    toggle,
    close: (panel) => untrack(() => close(panel)),
    history: (id, invoker) => untrack(() => showHistory(id, invoker)),
    swapResult: (id) => untrack(() => swapResult(id)),
    closeHistory: () => untrack(() => closeHistory()),
    legal: (invoker) => untrack(() => legal(invoker)),
    startOver: () => untrack(startOver),
    toggleRun: () => untrack(toggleRun),
    runWord: computed(() =>
      runAction(lingering(), app.run.running() || app.run.preparing(), app.run.phase()),
    ),
  };

  return {
    shell,
    elements,
    visited,
    workspace,
    panels,
    docked,
    flyout,
    legalOpen,
    historyOpen,
    selectedId,
    docks,
    resize,
    resetWidth,
    toggleHistory,
    returnToLive,
    toggleTheme,
    reset,
    confirmStartOver,
    closeLegal,
    legalInvoker: () => legalInvoker,
    /** Renders the location the page loaded with, a named panel taking focus; call once the console is in the page. */
    start: () =>
      untrack(() => {
        navigator.start();
        const last = panels.peek().at(-1);
        if (last && !legalOpen.peek()) title(last)?.focus({ preventScroll: true });
      }),
  };
}
