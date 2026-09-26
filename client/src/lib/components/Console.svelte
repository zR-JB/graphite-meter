<script lang="ts">
  import { observeWidth } from "../actions/observeWidth";
  // Main console shell: owns top-level panels, shortcuts,
  // theme toggle, and docked/flyout layout state.
  import { onMount, tick, type Component } from "svelte";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const { cancelPendingStart, hasPendingStart, returnToStart, toggleRun } =
    getApplicationController();
  import GaugePanel from "./GaugePanel.svelte";
  import ThroughputChart from "./ThroughputChart.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./settings/SettingsPanel.svelte";
  import TelemetryPanel from "./TelemetryPanel.svelte";
  import PhaseToast from "./PhaseToast.svelte";
  import ShortcutHints from "./ShortcutHints.svelte";
  import ConnectivityIndicator from "./ConnectivityIndicator.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import LegalDialog from "./LegalDialog.svelte";
  import TopbarMore from "./TopbarMore.svelte";
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";
  import { canFocus, activeModal } from "../actions/focus";
  import { MediaQuery } from "svelte/reactivity";
  import {
    resolveDockWidths,
    MIN_DOCK_WIDTH,
    MAX_DOCK_WIDTH,
    MIN_STAGE_WIDTH,
  } from "./dockWidths";
  import {
    DEFAULT_DOCK_WIDTH,
    loadPersisted,
    savePersisted,
  } from "../state/persistence";
  import { authEnabled } from "../auth";
  import { returnToLiveIndicator } from "../history/returnToLive";
  import {
    activatePanel,
    appRoute,
    closeDialog,
    closePanel,
    openDialog,
    parseRoute,
    serializeRoute,
    withWorkspace,
    type Route,
    type PanelSurface,
  } from "../router";

  let AccountControl = $state<Component | null>(null);
  let HistoryWorkspace = $state<Component<{
    selectedId: string | null;
    onNavigate: (id: string | null) => void;
    onClose: () => void;
  }> | null>(null);
  let historyChunkState = $state<"idle" | "loading" | "error">("idle");
  let historyInvoker: HTMLElement | null = null;
  let workspaceFocusIntent = $state<
    | {
        kind: "workspace";
        workspace: "history" | "measurement";
      }
    | {
        kind: "element";
        target: HTMLElement;
        workspace: "history" | "measurement";
      }
    | null
  >(null);
  const panelInvokers: Partial<Record<PanelSurface, HTMLElement>> = {};

  let resetConfirmOpen = $state(false);
  let legalInvoker = $state<HTMLElement | null>(null);
  let currentRoute = $state<Route>(
    parseRoute(typeof window === "undefined" ? "#/" : window.location.hash),
  );
  let historyOpen = $derived(
    currentRoute.kind === "app" && currentRoute.workspace.kind === "history",
  );
  let measurementOpen = $derived(
    currentRoute.kind === "app" &&
      currentRoute.workspace.kind === "measurement",
  );
  function loadHistoryWorkspace() {
    if (HistoryWorkspace || historyChunkState === "loading") return;
    historyChunkState = "loading";
    void import("./HistoryWorkspace.svelte")
      .then((module) => {
        HistoryWorkspace = module.default;
        historyChunkState = "idle";
      })
      .catch(() => (historyChunkState = "error"));
  }
  $effect(() => {
    if (historyOpen) loadHistoryWorkspace();
  });
  $effect(() => {
    const intent = workspaceFocusIntent;
    void currentRoute;
    void HistoryWorkspace;
    if (!intent) return;
    let cancelled = false;
    void tick().then(() => {
      if (cancelled || workspaceFocusIntent !== intent) return;
      const matchesRoute =
        intent.workspace === "history" ? historyOpen : measurementOpen;
      if (!matchesRoute) return;
      const modal = activeModal();
      if (
        modal &&
        (intent.kind !== "element" || !modal.contains(intent.target))
      ) {
        workspaceFocusIntent = null;
        return;
      }
      if (intent.kind === "element") {
        if (canFocus(intent.target)) {
          intent.target.focus({ preventScroll: true });
          workspaceFocusIntent = null;
          return;
        }
        const fallback = historyOpen ? "history" : "measurement";
        workspaceFocusIntent = { kind: "workspace", workspace: fallback };
        return;
      } else {
        const target = document.querySelector<HTMLElement>(
          intent.workspace === "history"
            ? ".history-workspace"
            : ".measurement-stage",
        );
        if (!target) return;
        target.focus({ preventScroll: true });
      }
      workspaceFocusIntent = null;
    });
    return () => {
      cancelled = true;
    };
  });
  let consoleWidth = $state(
    typeof window === "undefined" ? 0 : window.innerWidth,
  );
  const dockQuery = new MediaQuery("(min-width: 1200px)");
  const allowMultiplePanels = $derived(dockQuery.current);
  $effect(() => {
    const next = panelsForLayout(currentRoute);
    if (next !== currentRoute) routeTo(next, true);
  });
  const RESOLVED_PHASES = ["complete", "aborted", "error"];
  const awayRunIndicator = $derived.by(() => {
    if (measurementOpen) return null;
    const recovering = store.phaseStage
      ? store.stagePresentation[store.phaseStage].status === "recovering"
      : false;
    return returnToLiveIndicator(store.preparing, store.phase, recovering);
  });

  // Docked panels share the stage; flyouts have one active surface.
  const currentPanels = $derived(
    currentRoute.kind === "app" ? currentRoute.panels : [],
  );
  const lastPanel = $derived(currentPanels.at(-1));
  const settingsOpen = $derived(
    allowMultiplePanels
      ? currentPanels.includes("settings")
      : lastPanel === "settings",
  );
  const telemetryOpen = $derived(
    allowMultiplePanels
      ? currentPanels.includes("endpoint")
      : lastPanel === "endpoint",
  );
  const legalOpen = $derived(
    currentRoute.kind === "app" && currentRoute.dialog === "legal",
  );
  const lastOpened = $derived(lastPanel === "settings" ? "left" : "right");

  const THEME_CYCLE = ["light", "dark", "auto"] as const;
  const THEME_ICON: Record<(typeof THEME_CYCLE)[number], string> = {
    light: ICON.sun,
    dark: ICON.moon,
    auto: ICON.contrast,
  };
  const THEME_LABEL: Record<(typeof THEME_CYCLE)[number], string> = {
    light: "Light",
    dark: "Dark",
    auto: "Auto",
  };

  function toggleTheme() {
    const next =
      THEME_CYCLE[(THEME_CYCLE.indexOf(store.theme) + 1) % THEME_CYCLE.length];
    store.theme = next;
  }

  // Preferences survive viewport changes; the grid and resize controls share
  // the resolved widths, preserving room for the measurement instruments.
  const docks = $derived(
    resolveDockWidths(
      consoleWidth,
      dockQuery.current && settingsOpen ? store.dockWidth.left : 0,
      dockQuery.current && telemetryOpen ? store.dockWidth.right : 0,
    ),
  );
  const stageMinimum = $derived(
    Math.min(
      MIN_STAGE_WIDTH,
      consoleWidth -
        MIN_DOCK_WIDTH * (Number(settingsOpen) + Number(telemetryOpen)),
    ),
  );
  const dockMaxLeft = $derived(
    Math.min(MAX_DOCK_WIDTH, consoleWidth - stageMinimum - docks.right),
  );
  const dockMaxRight = $derived(
    Math.min(MAX_DOCK_WIDTH, consoleWidth - stageMinimum - docks.left),
  );

  let resizedDock = false;
  function setDockWidth(side: "left" | "right", px: number) {
    resizedDock = true;
    const other = side === "left" ? "right" : "left";
    // Freeze the visible sibling during an intentional resize so the handle
    // follows the pointer even when saved preferences were constrained.
    store.dockWidth = {
      ...store.dockWidth,
      ...(docks[other] ? { [other]: docks[other] } : {}),
      [side]: px,
    };
  }
  function resetDockWidth(side: "left" | "right") {
    resizedDock = true;
    store.dockWidth = { ...store.dockWidth, [side]: DEFAULT_DOCK_WIDTH[side] };
  }

  function requestReturnToStart() {
    if (!measurementOpen) {
      routeTo(withWorkspace(currentRoute, { kind: "measurement" }));
      return;
    }
    if (store.isRunning) {
      resetConfirmOpen = true;
      return;
    }
    returnToStart();
    routeTo(appRoute());
  }

  function panelsForLayout(next: Route): Route {
    return !allowMultiplePanels && next.kind === "app" && next.panels.length > 1
      ? { ...next, panels: next.panels.slice(-1) }
      : next;
  }
  function routeTo(next: Route, replace = false) {
    next = panelsForLayout(next);
    const parent = serializeRoute(currentRoute);
    commitRoute(next);
    const hash = serializeRoute(next);
    if (replace)
      window.history.replaceState({ graphiteRoute: false }, "", hash);
    else if (window.location.hash !== hash)
      window.history.pushState({ graphiteRoute: true, parent }, "", hash);
  }
  function backOrReplace(next: Route) {
    const marker = window.history.state as {
      graphiteRoute?: boolean;
      parent?: string;
    } | null;
    if (marker?.graphiteRoute && marker.parent === serializeRoute(next))
      window.history.back();
    else routeTo(next, true);
  }
  function historyRoute(
    id: string | null = null,
    invoker: HTMLElement | null = null,
  ) {
    if (id === null) historyInvoker = invoker;
    routeTo(withWorkspace(currentRoute, { kind: "history", selectedId: id }));
  }
  function focusWorkspace(workspace: "history" | "measurement") {
    workspaceFocusIntent = {
      kind: "workspace",
      workspace,
    };
  }
  function focusElement(
    target: HTMLElement,
    workspace: "history" | "measurement",
  ) {
    workspaceFocusIntent = {
      kind: "element",
      target,
      workspace,
    };
  }
  function closeHistory(focus: "measurement" | HTMLElement) {
    if (focus === "measurement") focusWorkspace("measurement");
    else focusElement(focus, "measurement");
    backOrReplace(withWorkspace(currentRoute, { kind: "measurement" }));
  }
  function auxiliaryOwnsFocus(route = currentRoute) {
    return (
      route.kind === "app" &&
      (route.dialog === "legal" ||
        (!dockQuery.current && route.panels.length > 0))
    );
  }
  function toggleHistoryFromPointer(invoker: HTMLElement) {
    if (historyOpen) {
      closeHistory(invoker);
    } else {
      historyRoute(null, invoker);
      focusElement(invoker, "history");
    }
  }
  function toggleHistoryFromShortcut() {
    const keepFocus = auxiliaryOwnsFocus();
    if (historyOpen) {
      historyInvoker = null;
      if (keepFocus)
        backOrReplace(withWorkspace(currentRoute, { kind: "measurement" }));
      else closeHistory("measurement");
    } else {
      historyRoute();
      if (!keepFocus) focusWorkspace("history");
    }
  }
  function dismissHistory() {
    const invoker = historyInvoker;
    historyInvoker = null;
    closeHistory(canFocus(invoker) ? invoker : "measurement");
  }
  function closeHistoryDetail() {
    backOrReplace(
      withWorkspace(currentRoute, { kind: "history", selectedId: null }),
    );
  }
  function panelRoute(panel: PanelSurface, invoker?: HTMLElement) {
    const replacingCompetingPanel =
      !allowMultiplePanels && lastPanel !== undefined && lastPanel !== panel;
    if (invoker) panelInvokers[panel] = invoker;
    else delete panelInvokers[panel];
    if (replacingCompetingPanel && lastPanel) delete panelInvokers[lastPanel];
    routeTo(activatePanel(currentRoute, panel), replacingCompetingPanel);
  }
  function dismissPanel(panel: PanelSurface, invoker?: HTMLElement) {
    if (invoker) panelInvokers[panel] = invoker;
    backOrReplace(closePanel(currentRoute, panel));
  }
  function togglePanelFromPointer(panel: PanelSurface, invoker: HTMLElement) {
    const open = panel === "settings" ? settingsOpen : telemetryOpen;
    if (open) dismissPanel(panel, invoker);
    else panelRoute(panel, invoker);
  }

  function confirmReturnToStart() {
    resetConfirmOpen = false;
    returnToStart();
    routeTo(appRoute());
  }

  function openLegal(invoker: HTMLElement) {
    legalInvoker = invoker;
    routeTo(openDialog(currentRoute, "legal"));
  }

  function closeLegal() {
    backOrReplace(closeDialog(currentRoute));
  }

  // Direct closes and browser navigation commit through the same focus owner.
  function commitRoute(next: Route, fromHistory = false) {
    const previous = currentRoute;
    const previousHistory = historyOpen;
    const nextHistory =
      next.kind === "app" && next.workspace.kind === "history";
    currentRoute = next;
    const workspace = nextHistory ? "history" : "measurement";
    if (workspaceFocusIntent && workspaceFocusIntent.workspace !== workspace)
      workspaceFocusIntent = null;
    const openingModal =
      next.kind === "app" &&
      ((next.dialog === "legal" &&
        (previous.kind !== "app" || previous.dialog !== "legal")) ||
        (!dockQuery.current &&
          next.panels.some(
            (panel) =>
              previous.kind !== "app" || !previous.panels.includes(panel),
          )));
    if (openingModal) {
      workspaceFocusIntent = null;
      return;
    }
    if (!workspaceFocusIntent && previous.kind === "app") {
      let invoker: HTMLElement | null | undefined;
      let closedSurface = false;
      if (
        previous.dialog === "legal" &&
        (next.kind !== "app" || next.dialog !== "legal")
      ) {
        invoker = legalInvoker;
        legalInvoker = null;
        closedSurface = true;
      } else {
        const nextPanels = next.kind === "app" ? next.panels : [];
        const removedPanel = [...previous.panels]
          .reverse()
          .find((panel) => !nextPanels.includes(panel));
        if (removedPanel) {
          invoker = panelInvokers[removedPanel];
          delete panelInvokers[removedPanel];
          closedSurface = true;
        }
      }
      if (closedSurface) {
        if (canFocus(invoker)) focusElement(invoker, workspace);
        else focusWorkspace(workspace);
        return;
      }
    }
    if (
      fromHistory &&
      previousHistory !== nextHistory &&
      !workspaceFocusIntent &&
      !auxiliaryOwnsFocus(next)
    )
      focusWorkspace(workspace);
  }

  function onBeforeUnload(e: BeforeUnloadEvent) {
    if (!store.isRunning) return;
    e.preventDefault();
    e.returnValue = "";
  }

  function isEditable(el: EventTarget | null): boolean {
    if (!(el instanceof HTMLElement)) return false;
    const tag = el.tagName;
    return (
      tag === "INPUT" ||
      tag === "TEXTAREA" ||
      tag === "SELECT" ||
      el.isContentEditable
    );
  }

  // Space and Enter already activate these natively.
  function selfActivating(el: EventTarget | null): boolean {
    if (!(el instanceof HTMLElement)) return false;
    const tag = el.tagName;
    return (
      tag === "BUTTON" ||
      tag === "A" ||
      tag === "SUMMARY" ||
      el.getAttribute("role") === "button"
    );
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey)
      return;
    if (document.querySelector(":popover-open")) return;
    if (isEditable(e.target)) return;
    if (resetConfirmOpen) return;

    if (e.key === "Escape") {
      if (
        currentRoute.kind === "app" &&
        currentRoute.workspace.kind === "history" &&
        currentRoute.workspace.selectedId
      ) {
        const detailClose = document.querySelector<HTMLButtonElement>(
          ".result-detail .close-detail",
        );
        if (detailClose) detailClose.click();
        else closeHistoryDetail();
      } else if (historyOpen) {
        dismissHistory();
      } else if (!measurementOpen) {
        return;
      } else if (store.isRunning) {
        toggleRun();
      } else if (hasPendingStart()) {
        cancelPendingStart();
      } else if (settingsOpen || telemetryOpen) {
        const requested = dockQuery.current
          ? lastOpened === "left"
            ? "settings"
            : "endpoint"
          : currentRoute.kind === "app"
            ? currentRoute.panels.at(-1)
            : undefined;
        if (requested === "settings" && settingsOpen) dismissPanel("settings");
        else if (requested === "endpoint" && telemetryOpen)
          dismissPanel("endpoint");
        else if (settingsOpen) dismissPanel("settings");
        else if (telemetryOpen) dismissPanel("endpoint");
      } else {
        return;
      }
      e.preventDefault();
      return;
    }

    if (e.key === " " || e.key === "Enter") {
      if (!measurementOpen || selfActivating(e.target)) return;
      toggleRun();
      e.preventDefault();
      return;
    }

    switch (e.key.toLowerCase()) {
      case "s":
        if (settingsOpen) {
          dismissPanel("settings");
        } else panelRoute("settings");
        e.preventDefault();
        break;
      case "d":
        if (telemetryOpen) {
          dismissPanel("endpoint");
        } else panelRoute("endpoint");
        e.preventDefault();
        break;
      case "h":
        toggleHistoryFromShortcut();
        e.preventDefault();
        break;
      case "r":
        if (measurementOpen && RESOLVED_PHASES.includes(store.phase)) {
          toggleRun();
          e.preventDefault();
        }
        break;
      case "t":
        toggleTheme();
        e.preventDefault();
        break;
    }
  }

  onMount(() => {
    // Preserve an intentional dock resize even if reload beats the normal
    // debounced preference save. Leave other persisted preferences untouched.
    const saveDockWidths = () => {
      if (resizedDock)
        savePersisted({
          ...loadPersisted(),
          dockWidth: $state.snapshot(store.dockWidth),
        });
    };
    window.addEventListener("pagehide", saveDockWidths);
    const onHashChange = () => {
      const next = panelsForLayout(parseRoute(window.location.hash));
      if (serializeRoute(next) !== window.location.hash)
        window.history.replaceState(
          { graphiteRoute: false },
          "",
          serializeRoute(next),
        );
      commitRoute(next, true);
    };
    window.addEventListener("hashchange", onHashChange);
    window.addEventListener("popstate", onHashChange);
    onHashChange();
    window.addEventListener("keydown", onKeydown);
    window.addEventListener("beforeunload", onBeforeUnload);
    if (authEnabled)
      void import("./AccountControl.svelte")
        .then((m) => (AccountControl = m.default))
        .catch(() => {
          // The optional account control may be unavailable with an offline
          // lazy chunk; the measurement UI remains usable without it.
          AccountControl = null;
        });

    return () => {
      window.removeEventListener("pagehide", saveDockWidths);
      window.removeEventListener("keydown", onKeydown);
      window.removeEventListener("hashchange", onHashChange);
      window.removeEventListener("popstate", onHashChange);
      window.removeEventListener("beforeunload", onBeforeUnload);
    };
  });
</script>

<main
  id="console"
  {@attach observeWidth((width) => (consoleWidth = width))}
  data-phase={store.phase}
  style="--dock-left: {docks.left}px; --dock-right: {docks.right}px;"
>
  <!-- Topbar: a size container. Its queries move the direct actions into the
       More menu as the bar narrows, without measuring any children. -->
  <header class="topbar" class:saving={store.savingResults}>
    <button
      type="button"
      class="brand-btn"
      aria-label={measurementOpen
        ? "Graphite Meter — return to a fresh, blank test"
        : "Graphite Meter — return to live meter"}
      use:tooltip={measurementOpen
        ? "Return to a fresh, blank test"
        : "Return to live meter"}
      onclick={requestReturnToStart}
      ><svg class="brand-glyph" viewBox="0 0 24 24" aria-hidden="true"
        ><path
          d="M12 2.6 3.9 7.3v9.4l8.1 4.7 8.1-4.7V7.3Z"
          fill="none"
          stroke="var(--brand)"
          stroke-width="2"
          stroke-linejoin="round"
        /><path
          d="M12 12 18.6 8.2"
          stroke="currentColor"
          stroke-width="2.2"
          stroke-linecap="round"
        /><circle cx="12" cy="12" r="2.1" fill="currentColor" /></svg
      ><span class="brand-label">Graphite&nbsp;Meter</span></button
    >
    <button
      class="btn btn-icon"
      aria-label="Open settings"
      aria-expanded={settingsOpen}
      use:tooltip={"Settings — test and display (S)"}
      onclick={(event) =>
        togglePanelFromPointer("settings", event.currentTarget as HTMLElement)}
      >{@html ICON.settings}</button
    >
    <span class="chrome-divider" aria-hidden="true"></span>
    <div class="connectivity"><ConnectivityIndicator /></div>
    <div class="topbar-spacer"></div>
    {#if awayRunIndicator}<button
        class="btn return-live"
        data-tone={awayRunIndicator.tone}
        type="button"
        aria-label={`${awayRunIndicator.label}. Return to live meter.`}
        use:tooltip={`${awayRunIndicator.label} — return to live meter`}
        onclick={() => {
          focusWorkspace("measurement");
          routeTo(withWorkspace(currentRoute, { kind: "measurement" }));
        }}
      >
        <span class="run-icon">{@html ICON[awayRunIndicator.icon]}</span>
        <span class="live-copy">
          <strong>Live</strong>
          <span aria-hidden="true">·</span>
          <span>{awayRunIndicator.label}</span>
        </span>
      </button>{/if}
    {#if AccountControl}<AccountControl />{/if}
    {#if store.savingResults}<button
        class="btn btn-icon direct-history"
        type="button"
        aria-label={historyOpen ? "Close History" : "Open History"}
        aria-current={historyOpen ? "page" : undefined}
        aria-pressed={historyOpen}
        use:tooltip={historyOpen ? "Close History" : "History — saved results"}
        onclick={(event) =>
          toggleHistoryFromPointer(event.currentTarget as HTMLElement)}
        >{@html ICON.history}</button
      >{/if}
    <button
      class="btn btn-icon direct-theme"
      aria-label={`Theme: ${THEME_LABEL[store.theme]}. Click to cycle light / dark / auto.`}
      use:tooltip={`Theme: ${THEME_LABEL[store.theme]} (T) — cycles light / dark / auto`}
      onclick={toggleTheme}>{@html THEME_ICON[store.theme]}</button
    >
    <button
      class="btn btn-icon direct-endpoint"
      aria-label="Toggle endpoint info"
      aria-expanded={telemetryOpen}
      use:tooltip={"Endpoint info"}
      onclick={(event) =>
        togglePanelFromPointer("endpoint", event.currentTarget as HTMLElement)}
      >{@html ICON.info}</button
    >
    <div class="topbar-more">
      <TopbarMore
        showHistory={store.savingResults}
        historyActive={historyOpen}
        endpointActive={telemetryOpen}
        theme={store.theme}
        onHistory={toggleHistoryFromPointer}
        onEndpoint={(invoker: HTMLElement) =>
          togglePanelFromPointer("endpoint", invoker)}
        onTheme={toggleTheme}
      />
    </div>
  </header>

  <!-- Centre stage: a height-bounded column. The gauge hero takes most of it
       and the chart stays compact, so the default fits without scrolling. -->
  {#if currentRoute.kind === "not-found"}
    <section class="stage history-stage">
      <div class="empty-state">
        <h1>Page not found</h1>
        <p>That client route does not exist.</p>
        <a class="btn btn-accent" href="#/">Return to measurement</a>
      </div>
    </section>
  {:else if historyOpen}
    <section class="stage history-stage">
      {#if HistoryWorkspace}<HistoryWorkspace
          selectedId={currentRoute.kind === "app" &&
          currentRoute.workspace.kind === "history"
            ? currentRoute.workspace.selectedId
            : null}
          onNavigate={(id: string | null) =>
            id ? historyRoute(id) : closeHistoryDetail()}
          onClose={dismissHistory}
        />{:else if historyChunkState === "error"}<div
          class="empty-state"
          data-tone="err"
          role="alert"
        >
          <span class="empty-icon">!</span>
          <p>History could not be opened.</p>
          <button
            class="btn btn-accent"
            type="button"
            onclick={loadHistoryWorkspace}>Retry</button
          >
        </div>{:else}<div class="empty-state" role="status">
          <span class="empty-icon">{@html ICON.history}</span>Opening local
          archive…
        </div>{/if}
    </section>
  {:else}
    <section
      class="stage measurement-stage"
      aria-label="Measurement workspace"
      tabindex="-1"
    >
      <GaugePanel /><ThroughputChart />
    </section>
  {/if}

  <footer class="status">
    <StatusBar />
    <ShortcutHints />
  </footer>

  <!-- Auxiliary panels: one shared base, opposite sides. They dock on wide
       screens (pushing the stage) and overlay as flyouts below that. Docked
       panels resize from their inner edge, persisted via store.dockWidth. -->
  <SettingsPanel
    open={settingsOpen}
    docked={dockQuery.current}
    raised={lastOpened === "left"}
    dockWidth={docks.left}
    dockMaxWidth={dockMaxLeft}
    onResize={(px) => setDockWidth("left", px)}
    onResetWidth={() => resetDockWidth("left")}
    onClose={() => dismissPanel("settings")}
    onOpenHistory={(invoker: HTMLElement) => historyRoute(null, invoker)}
  />
  <TelemetryPanel
    open={telemetryOpen}
    docked={dockQuery.current}
    raised={lastOpened === "right"}
    dockWidth={docks.right}
    dockMaxWidth={dockMaxRight}
    onResize={(px) => setDockWidth("right", px)}
    onResetWidth={() => resetDockWidth("right")}
    onClose={() => dismissPanel("endpoint")}
    onOpenLegal={openLegal}
  />

  <PhaseToast />

  <ConfirmDialog
    open={resetConfirmOpen}
    id="reset-confirm"
    title="Stop the running test?"
    description="Returning to a fresh test will abort the measurement in progress."
    cancelLabel="Keep running"
    confirmLabel="Stop test"
    onCancel={() => (resetConfirmOpen = false)}
    onConfirm={confirmReturnToStart}
  />

  <LegalDialog open={legalOpen} onClose={closeLegal} />
</main>

<style>
  /* Console grid: the stage owns the middle; the dock columns are 0-width
     until a panel docks on a wide screen. A docked <SidePanel> reaches
     leftdock/rightdock through display:contents and pushes the stage. */
  #console {
    display: grid;
    grid-template-columns: var(--dock-left, 0px) minmax(0, 1fr) var(
        --dock-right,
        0px
      );
    grid-template-rows:
      var(--topbar-h) minmax(0, 1fr)
      calc(var(--statusbar-h) + env(safe-area-inset-bottom, 0px));
    grid-template-areas:
      "topbar   topbar  topbar"
      "leftdock stage   rightdock"
      "status   status  status";
    height: 100dvh;
    background: var(--bg);
    color: var(--text);
  }

  .topbar {
    grid-area: topbar;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-inline: var(--space-4);
    border-bottom: 1px solid var(--border);
    container: topbar / inline-size;
  }
  .topbar > :global(*) {
    flex-shrink: 0;
  }
  .topbar-spacer {
    flex: 1;
    min-width: 0;
  }
  /* The logo doubles as the home action: the wordmark with a hover and
     focus affordance. */
  .brand-btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: var(--space-1) 6px;
    margin-left: -6px;
    border-radius: var(--r-chrome);
    font: 700 var(--type-md) / 1.4 var(--font-mono);
    letter-spacing: -0.025em;
    transition: color var(--dur-hover) var(--ease-out);
  }
  @media (hover: hover) {
    .brand-btn:hover {
      color: var(--brand-strong);
    }
  }
  /* Hexagon in the brand accent, needle in the text colour (favicon.svg). */
  .brand-glyph {
    width: 18px;
    height: 18px;
  }
  .chrome-divider {
    width: 1px;
    height: 22px;
    margin: 0 2px;
    background: var(--border);
  }
  .connectivity {
    display: grid;
    place-items: center;
  }
  .return-live {
    --btn-line: var(--tone-line);
    padding-inline: var(--space-2) 10px;
    background-color: color-mix(in srgb, var(--tone) 9%, var(--surface-2));
  }
  @media (hover: hover) {
    .return-live:hover {
      --btn-line: var(--tone);
    }
  }
  .run-icon {
    display: grid;
    color: var(--tone);
  }
  .live-copy {
    display: inline-flex;
    align-items: baseline;
    gap: var(--space-1);
  }
  .live-copy strong {
    color: var(--tone);
    font-weight: 800;
  }
  /* Overflow: 44px coarse targets leave three direct actions room down to a
     ~320px bar, one down to ~260px, then everything moves into More. */
  .topbar-more,
  .topbar :global([data-more]) {
    display: none;
  }
  @container topbar (max-width: 319px) {
    .saving :is(.direct-history, .direct-endpoint) {
      display: none;
    }
    .saving .topbar-more {
      display: block;
    }
    .saving :global(:is([data-more="history"], [data-more="endpoint"])) {
      display: grid;
    }
  }
  @container topbar (max-width: 259px) {
    .direct-history,
    .direct-endpoint,
    .direct-theme {
      display: none;
    }
    .topbar-more {
      display: block;
    }
    .topbar :global([data-more]) {
      display: grid;
    }
  }

  .stage {
    grid-area: stage;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
    padding: var(--space-2) var(--space-3);
    overflow-y: auto;
    /* Keep stage scrolling from chaining out to the document. */
    overscroll-behavior: contain;
  }
  .history-stage {
    overflow: hidden;
  }
  .measurement-stage:focus {
    outline: none;
  }
  /* The gauge owns its mode-stable intrinsic height; a viewport too short for
     the complete stage scrolls this column beneath the anchored chrome. */
  .stage > :global(:is(.gauge-panel, .chart)) {
    width: 100%;
    max-width: 1920px;
    align-self: center;
  }
  .stage > :global(.gauge-panel) {
    flex: none;
  }
  /* The timeline uses spare height while the gauge remains stable. */
  .stage > :global(.chart) {
    flex: 1 0 160px;
    min-height: 160px;
    max-height: 360px;
  }
  @media (max-height: 800px) {
    .measurement-stage {
      gap: var(--space-2);
      padding-block: var(--space-1);
    }
    .stage > :global(.chart) {
      flex-basis: 120px;
      min-height: 120px;
    }
  }
  .status {
    grid-area: status;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
    overflow: hidden;
    padding: 0 var(--space-4) env(safe-area-inset-bottom, 0px);
    border-top: 1px solid var(--border);
    background: var(--surface-1);
    color: var(--text-soft);
    font: var(--type-xs) var(--font-mono);
    container: status / inline-size;
  }

  @media (max-width: 759px) {
    .topbar {
      gap: 2px;
      padding-inline: 6px;
    }
    .brand-label,
    .chrome-divider,
    .live-copy {
      display: none;
    }
    .brand-btn {
      justify-content: center;
      min-width: var(--hit);
      min-height: var(--hit);
      margin: 0;
      padding: 0;
    }
    .return-live {
      width: calc(var(--control-h) + 2 * var(--hit-pad));
      padding: 0;
    }
    .connectivity {
      width: 24px;
    }
    .connectivity :global(.spark) {
      display: none;
    }
  }
</style>
