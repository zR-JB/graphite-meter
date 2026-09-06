<script lang="ts">
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
  import ServerChooser from "./ServerChooser.svelte";
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
  import { mediaQuery } from "../actions/mediaQuery.svelte";
  import {
    resolveDockWidths,
    MIN_DOCK_WIDTH,
    MAX_DOCK_WIDTH,
    MIN_STAGE_WIDTH,
  } from "./dockWidths";
  import { DEFAULT_DOCK_WIDTH } from "../state/persistence";
  import { authEnabled } from "../auth";
  import { returnToLiveIndicator } from "../history/returnToLive";
  import {
    activatePanel,
    appRoute,
    closeDialog,
    closePanel,
    openDialog,
    parseRoute,
    reconcilePanels,
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
  const allowMultiplePanels = $derived(
    consoleWidth >= MIN_STAGE_WIDTH + 2 * MIN_DOCK_WIDTH,
  );
  const dockQuery = mediaQuery(`(min-width: 1200px)`);
  const secondaryActionsQuery = mediaQuery(
    `(min-width: 340px) and (not (pointer: coarse)), (min-width: 430px)`,
  );
  const themeDirectQuery = mediaQuery(
    `(min-width: 320px) and (not (pointer: coarse)), (min-width: 360px)`,
  );
  const liveDirectComfortQuery = mediaQuery(
    `(min-width: 380px) and (not (pointer: coarse)), (min-width: 560px)`,
  );
  const RESOLVED_PHASES = ["complete", "aborted", "error"];
  const awayRunIndicator = $derived.by(() => {
    if (measurementOpen) return null;
    const recovering = store.phaseStage
      ? store.stagePresentation[store.phaseStage].status === "recovering"
      : false;
    return returnToLiveIndicator(store.preparing, store.phase, recovering);
  });

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

  // Keep one authoritative panel when two docks would crowd the instruments;
  // its canonical route also stays valid when the panel becomes a flyout.
  $effect(() => {
    const reconciled = reconcilePanels(currentRoute, allowMultiplePanels);
    if (serializeRoute(reconciled) !== serializeRoute(currentRoute))
      routeTo(reconciled, true);
  });

  const showHistoryDirect = $derived(
    secondaryActionsQuery.matches &&
      (awayRunIndicator === null || liveDirectComfortQuery.matches),
  );
  const showEndpointDirect = $derived(
    secondaryActionsQuery.matches &&
      (awayRunIndicator === null || liveDirectComfortQuery.matches),
  );
  const showThemeDirect = $derived(themeDirectQuery.matches);
  const historyInMore = $derived(store.savingResults && !showHistoryDirect);
  const endpointInMore = $derived(!showEndpointDirect);
  const themeInMore = $derived(!showThemeDirect);
  const moreAvailable = $derived(
    historyInMore || endpointInMore || themeInMore,
  );

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
      dockQuery.matches && settingsOpen ? store.dockWidth.left : 0,
      dockQuery.matches && telemetryOpen ? store.dockWidth.right : 0,
    ),
  );
  const dockMaxLeft = $derived(
    Math.min(MAX_DOCK_WIDTH, consoleWidth - MIN_STAGE_WIDTH - docks.right),
  );
  const dockMaxRight = $derived(
    Math.min(MAX_DOCK_WIDTH, consoleWidth - MIN_STAGE_WIDTH - docks.left),
  );

  function setDockWidth(side: "left" | "right", px: number) {
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

  function routeTo(next: Route, replace = false) {
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
        (!dockQuery.matches && route.panels.length > 0))
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
    routeTo(
      activatePanel(currentRoute, panel, allowMultiplePanels),
      replacingCompetingPanel,
    );
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
        (!dockQuery.matches &&
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
        const requested = dockQuery.matches
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
    const onHashChange = () =>
      commitRoute(parseRoute(window.location.hash), true);
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
      window.removeEventListener("keydown", onKeydown);
      window.removeEventListener("hashchange", onHashChange);
      window.removeEventListener("popstate", onHashChange);
      window.removeEventListener("beforeunload", onBeforeUnload);
    };
  });
</script>

<main
  id="console"
  bind:clientWidth={consoleWidth}
  data-phase={store.phase}
  style="--dock-left: {docks.left}px; --dock-right: {docks.right}px;"
  class="bg-bg text-text"
>
  <!-- TOPBAR -->
  <header
    class="zone topbar flex items-center gap-3 border-b border-border px-4"
    class:authenticated={authEnabled}
  >
    <button
      type="button"
      class="brand-btn font-mono text-sm font-bold tracking-tight"
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
      class="ghost-btn icon-btn"
      aria-label="Open settings"
      aria-expanded={settingsOpen}
      use:tooltip={"Settings — test and display (S)"}
      onclick={(event) =>
        togglePanelFromPointer("settings", event.currentTarget as HTMLElement)}
      >{@html ICON.settings}</button
    >
    <span class="chrome-divider" aria-hidden="true"></span>
    <div class="connectivity"><ConnectivityIndicator /></div>
    <div class="flex-1"></div>
    {#if awayRunIndicator}<button
        class="return-live"
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
          <span class="live-separator" aria-hidden="true">·</span>
          <span class="live-phase">{awayRunIndicator.label}</span>
        </span>
      </button>{/if}
    {#if AccountControl}<AccountControl />{/if}
    {#if store.savingResults && showHistoryDirect}<button
        class="ghost-btn icon-btn"
        type="button"
        aria-label={historyOpen ? "Close History" : "Open History"}
        aria-current={historyOpen ? "page" : undefined}
        aria-pressed={historyOpen}
        use:tooltip={historyOpen ? "Close History" : "History — saved results"}
        onclick={(event) =>
          toggleHistoryFromPointer(event.currentTarget as HTMLElement)}
        >{@html ICON.history}</button
      >{/if}
    {#if showThemeDirect}
      <button
        class="ghost-btn icon-btn"
        aria-label={`Theme: ${THEME_LABEL[store.theme]}. Click to cycle light / dark / auto.`}
        use:tooltip={`Theme: ${THEME_LABEL[store.theme]} (T) — cycles light / dark / auto`}
        onclick={toggleTheme}>{@html THEME_ICON[store.theme]}</button
      >
    {/if}
    {#if showEndpointDirect}
      <button
        class="ghost-btn icon-btn"
        aria-label="Toggle endpoint info"
        aria-expanded={telemetryOpen}
        use:tooltip={"Endpoint info"}
        onclick={(event) =>
          togglePanelFromPointer(
            "endpoint",
            event.currentTarget as HTMLElement,
          )}>{@html ICON.info}</button
      >
    {/if}
    {#if moreAvailable}
      <TopbarMore
        showHistory={historyInMore}
        showEndpoint={endpointInMore}
        showTheme={themeInMore}
        historyActive={historyOpen}
        endpointActive={telemetryOpen}
        theme={store.theme}
        onHistory={toggleHistoryFromPointer}
        onEndpoint={(invoker: HTMLElement) =>
          togglePanelFromPointer("endpoint", invoker)}
        onTheme={toggleTheme}
      />
    {/if}
  </header>

  <!-- Center stage: a height-bounded flex column. The gauge hero takes most of
       it, the chart stays compact so the default fits the viewport without
       vertical scroll. Advancing a phase never resizes this section. -->
  {#if currentRoute.kind === "not-found"}
    <section
      class="zone stage history-stage flex min-w-0 flex-col overflow-y-auto"
    >
      <div class="route-not-found">
        <h1>Page not found</h1>
        <p>That client route does not exist.</p>
        <a href="#/">Return to measurement</a>
      </div>
    </section>
  {:else if historyOpen}
    <section
      class="zone stage history-stage flex min-w-0 flex-col overflow-y-auto"
    >
      {#if HistoryWorkspace}<HistoryWorkspace
          selectedId={currentRoute.kind === "app" &&
          currentRoute.workspace.kind === "history"
            ? currentRoute.workspace.selectedId
            : null}
          onNavigate={(id: string | null) =>
            id ? historyRoute(id) : closeHistoryDetail()}
          onClose={dismissHistory}
        />{:else if historyChunkState === "error"}<div
          class="history-loading error"
          role="alert"
        >
          <span>!</span>
          <p>History could not be opened.</p>
          <button type="button" onclick={loadHistoryWorkspace}>Retry</button>
        </div>{:else}<div class="history-loading" role="status">
          <span>{@html ICON.history}</span>Opening local archive…
        </div>{/if}
    </section>
  {:else}
    <section
      class="zone stage measurement-stage flex min-w-0 flex-col overflow-y-auto"
      aria-label="Measurement workspace"
      tabindex="-1"
    >
      <GaugePanel /><ThroughputChart />
    </section>
  {/if}

  <!-- STATUS BAR -->
  <footer
    class="zone status flex items-center gap-4 border-t border-border bg-surface-1 px-4 font-mono text-soft"
  >
    <StatusBar />
    <ShortcutHints />
  </footer>

  <!-- Auxiliary panels: one shared base, opposite sides. They dock on wide
       screens (pushing the stage) and overlay as flyouts below that. Docked
       panels resize from their inner edge, persisted via store.dockWidth. -->
  <SettingsPanel
    open={settingsOpen}
    docked={dockQuery.matches}
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
    docked={dockQuery.matches}
    raised={lastOpened === "right"}
    dockWidth={docks.right}
    dockMaxWidth={dockMaxRight}
    onResize={(px) => setDockWidth("right", px)}
    onResetWidth={() => resetDockWidth("right")}
    onClose={() => dismissPanel("endpoint")}
    onOpenLegal={openLegal}
  />

  <!-- Transient phase-change toast, pinned bottom-right. -->
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
  <ServerChooser />
</main>

<style>
  /* Console grid: the stage owns the middle, the dock columns are 0-width
     until a panel docks on a wide screen. A docked <SidePanel> reaches
     leftdock/rightdock through display:contents and pushes the stage. Below
     the dock breakpoint the panels are flyout overlays. */
  #console {
    display: grid;
    /* One shared width budget reserves the instrument stage. */
    grid-template-columns:
      var(--dock-left, 0px)
      minmax(0, 1fr)
      var(--dock-right, 0px);
    grid-template-rows:
      var(--topbar-h) minmax(0, 1fr)
      var(--statusbar-h);
    grid-template-areas:
      "topbar   topbar  topbar"
      "leftdock stage   rightdock"
      "status   status  status";
    height: 100dvh;
    gap: 0;
  }

  .topbar {
    grid-area: topbar;
  }
  .stage {
    grid-area: stage;
    /* The hero gauge flexes to fill, chart and chips keep their compact
       intrinsic height. This is the flat faceplate, the wells carry the depth. */
    padding: var(--space-2) var(--space-3);
    gap: var(--space-3);
    /* Keep stage scrolling from chaining out to the document (anchored bars). */
    overscroll-behavior: contain;
  }
  .history-stage {
    overflow: hidden;
  }
  /* The gauge owns its mode-stable intrinsic height. Any viewport too short
     for the complete stage scrolls this center column beneath anchored chrome. */
  .stage > :global(.gauge-panel) {
    flex: 0 0 auto;
    min-height: 0;
  }
  .stage > :global(.gauge-panel),
  .stage > :global(.chart) {
    width: 100%;
    max-width: 1920px;
    align-self: center;
  }
  /* Let the timeline use spare height while the gauge remains mode-stable. */
  .stage > :global(.chart) {
    flex: 1 0 160px;
    min-height: 160px;
    max-height: 360px;
  }
  @media (max-height: 800px) {
    .measurement-stage {
      padding-block: var(--space-1);
      gap: var(--space-2);
    }
    .stage > :global(.chart) {
      flex-basis: 120px;
      min-height: 120px;
    }
  }
  .status {
    grid-area: status;
    font-size: 11px;
    /* A 28px chrome strip. Clip whatever cannot fit: at awkward widths the
       text spills out or wraps past the row. */
    min-width: 0;
    overflow: hidden;
  }

  /* The logo doubles as a "home" action: it reads as the wordmark, with a
     hover/focus affordance to signal it is clickable. */
  .brand-btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 4px 6px;
    margin-left: -6px;
    border: 0;
    border-radius: var(--r-chrome);
    background: transparent;
    color: var(--text);
    cursor: pointer;
    transition: color var(--dur-hover) var(--ease-out);
  }
  /* The lattice-needle glyph (see public/favicon.svg): hexagon in the brand
     accent, needle in the current text color so hover tints it with the
     wordmark. */
  .brand-btn .brand-glyph {
    width: 18px;
    height: 18px;
    flex: none;
  }
  .brand-btn:hover {
    color: var(--brand-strong);
  }
  .brand-btn:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }

  /* Flat chrome on the faceplate: a milled top edge-highlight gives the
     button a tactile lift without a floating drop shadow. */
  .ghost-btn {
    display: grid;
    place-items: center;
    min-width: 32px;
    height: 32px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: inset 0 1px 0 var(--edge-light);
    color: var(--text-muted);
    font-size: var(--type-md);
    transition:
      border-color var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .ghost-btn:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .icon-btn :global(svg) {
    width: 16px;
    height: 16px;
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
    --run-tone: var(--phase-warmup);
    display: inline-flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    height: 32px;
    padding: 0 10px 0 8px;
    border: 1px solid color-mix(in srgb, var(--run-tone) 52%, var(--border));
    border-radius: var(--r-chrome);
    background: color-mix(in srgb, var(--run-tone) 9%, var(--surface-2));
    box-shadow: inset 0 1px 0 var(--edge-light);
    color: var(--text-muted);
    font-size: var(--type-xs);
    font-weight: 700;
    cursor: pointer;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .return-live[data-tone="latency"] {
    --run-tone: var(--phase-latency);
  }
  .return-live[data-tone="download"] {
    --run-tone: var(--phase-download);
  }
  .return-live[data-tone="upload"] {
    --run-tone: var(--phase-upload);
  }
  .return-live[data-tone="bidirectional"] {
    --run-tone: var(--phase-bidirectional);
  }
  .return-live:hover {
    border-color: var(--run-tone);
    background: color-mix(in srgb, var(--run-tone) 14%, var(--surface-2));
    color: var(--text);
  }
  .run-icon {
    display: grid;
    place-items: center;
    flex: none;
    color: var(--run-tone);
  }
  .run-icon :global(svg) {
    width: 15px;
    height: 15px;
  }
  .live-copy {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
    min-width: 0;
    white-space: nowrap;
  }
  .live-copy strong {
    color: var(--run-tone);
    font-weight: 800;
  }
  .history-loading,
  .route-not-found {
    display: grid;
    justify-items: center;
    align-content: center;
    flex: 1;
    min-height: 280px;
    gap: var(--space-3);
    color: var(--text-muted);
    text-align: center;
  }
  .history-loading > span {
    display: grid;
    place-items: center;
    width: 42px;
    height: 42px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    color: var(--brand);
  }
  .history-loading > span :global(svg) {
    width: 20px;
    height: 20px;
  }
  .history-loading.error > span {
    color: var(--err);
    font-weight: 800;
  }
  .history-loading p,
  .route-not-found p {
    margin: 0;
  }
  .measurement-stage:focus {
    outline: none;
  }
  .history-loading button,
  .route-not-found a {
    min-height: 30px;
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    color: var(--brand-strong);
    font-size: var(--type-xs);
    font-weight: 700;
    text-decoration: none;
  }
  .route-not-found h1 {
    margin: 0;
    font-size: var(--type-xl);
  }

  /* Portrait phones are the single document-flow mode. Landscape phones use
     the same anchored shell and scrollable center stage as tablets, so
     rotation cannot silently swap in a bottom sheet or clip the status bar. */
  @media (max-width: 759px) and (orientation: portrait) {
    #console {
      height: auto;
      min-height: 100svh;
    }
    .stage {
      overflow-y: visible;
      overscroll-behavior: auto;
    }
  }
  @media (max-width: 520px) {
    .topbar {
      gap: var(--space-2);
      padding-inline: var(--space-2);
    }
    .brand-label {
      display: none;
    }
    .brand-btn {
      margin-left: -2px;
    }
    .return-live {
      padding-inline: 7px;
    }
    .live-separator,
    .live-phase {
      display: none;
    }
    .live-copy {
      max-width: 48px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
  }
  @media (max-width: 430px) {
    .connectivity :global(.spark) {
      display: none;
    }
    .return-live {
      gap: 5px;
      padding-inline: 6px;
    }
  }
  @media (pointer: coarse) and (max-width: 640px) {
    .brand-label {
      display: none;
    }
  }
  @media (pointer: coarse) {
    .topbar :global(.account) {
      min-width: 44px;
    }
    .topbar :global(button) {
      min-width: 44px;
      min-height: 44px;
      flex-shrink: 0;
    }
  }
</style>
