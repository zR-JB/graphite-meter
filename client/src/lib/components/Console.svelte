<script lang="ts">
  import Icon from "./Icon.svelte";
  import { observeWidth } from "../actions/observeWidth";
  import { onMount, tick, type Component } from "svelte";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const { cancelPendingStart, hasPendingStart, returnToStart, toggleRun } =
    getApplicationController();
  import GaugePanel from "./GaugePanel.svelte";
  import ThroughputChart from "./ThroughputChart.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SidePanel from "./SidePanel.svelte";
  import TestSetupPanel from "./settings/TestSetupPanel.svelte";
  import EndpointInfo from "./EndpointInfo.svelte";
  import PhaseToast from "./PhaseToast.svelte";
  import ShortcutHints from "./ShortcutHints.svelte";
  import ConnectivityIndicator from "./ConnectivityIndicator.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import LegalDialog from "./LegalDialog.svelte";
  import TopbarMore from "./TopbarMore.svelte";
  import { resolvedPhase, THEME } from "../presentation/vocabulary";
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
  import { authEnabled as pageAuthEnabled } from "../auth";
  const authEnabled = pageAuthEnabled();
  import { returnToLiveIndicator } from "../history/returnToLive";
  import { announcements } from "../presentation/announcer.svelte";
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
  let historyChunkFailed = $state(false);
  let historyChunk: Promise<unknown> | undefined;
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
    historyChunk ??= import("./HistoryWorkspace.svelte")
      .then((module) => (HistoryWorkspace = module.default))
      .catch(() => (historyChunkFailed = true));
  }
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

  const THEME_CYCLE = ["light", "dark", "auto"] as const;

  function toggleTheme() {
    const next =
      THEME_CYCLE[(THEME_CYCLE.indexOf(store.theme) + 1) % THEME_CYCLE.length];
    store.prefer({ theme: next });
  }

  // The grid and resize controls share one resolution of the saved widths.
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
    // Freeze the visible sibling so the handle tracks the pointer.
    store.prefer({
      dockWidth: {
        ...store.dockWidth,
        ...(docks[other] ? { [other]: docks[other] } : {}),
        [side]: px,
      },
    });
  }
  function resetDockWidth(side: "left" | "right") {
    resizedDock = true;
    store.prefer({
      dockWidth: { ...store.dockWidth, [side]: DEFAULT_DOCK_WIDTH[side] },
    });
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
  function togglePanel(panel: PanelSurface, invoker?: HTMLElement) {
    const open = panel === "settings" ? settingsOpen : telemetryOpen;
    if (open) dismissPanel(panel, invoker);
    else panelRoute(panel, invoker);
  }

  function confirmReturnToStart() {
    resetConfirmOpen = false;
    returnToStart();
    routeTo(appRoute());
  }

  function openLegal() {
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
    if (nextHistory) loadHistoryWorkspace();
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
    const nextPanels = next.kind === "app" ? next.panels : [];
    const removedPanel =
      previous.kind === "app" &&
      previous.panels.findLast((panel) => !nextPanels.includes(panel));
    if (!workspaceFocusIntent && removedPanel) {
      const invoker = panelInvokers[removedPanel];
      delete panelInvokers[removedPanel];
      if (canFocus(invoker)) focusElement(invoker, workspace);
      else focusWorkspace(workspace);
      return;
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

  // Keys belong to text entry; a focused toggle leaves them to the page.
  const isEditable = (el: EventTarget | null) =>
    el instanceof HTMLElement &&
    (el.isContentEditable ||
      el.matches("textarea, select, input:not([type=checkbox], [type=radio])"));

  // Space and Enter belong to whatever control holds focus.
  function unownedTarget(el: EventTarget | null): boolean {
    return (
      el === document.body ||
      (el instanceof HTMLElement && el.classList.contains("measurement-stage"))
    );
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey)
      return;
    if (document.querySelector(":popover-open:not(.tooltip)")) return;
    if (isEditable(e.target) || activeModal()) return;

    if (e.key === "Escape") {
      if (
        currentRoute.kind === "app" &&
        currentRoute.workspace.kind === "history" &&
        currentRoute.workspace.selectedId
      ) {
        closeHistoryDetail();
      } else if (historyOpen) {
        dismissHistory();
      } else if (!measurementOpen) {
        return;
      } else if (lastPanel) {
        dismissPanel(lastPanel);
      } else if (store.isRunning) {
        toggleRun();
      } else if (hasPendingStart()) {
        cancelPendingStart();
      } else {
        return;
      }
      e.preventDefault();
      return;
    }

    if (e.key === " " || e.key === "Enter") {
      if (!measurementOpen || !unownedTarget(e.target)) return;
      toggleRun();
      e.preventDefault();
      return;
    }

    switch (e.key.toLowerCase()) {
      case "s":
        togglePanel("settings");
        e.preventDefault();
        break;
      case "d":
        togglePanel("endpoint");
        e.preventDefault();
        break;
      case "h":
        toggleHistoryFromShortcut();
        e.preventDefault();
        break;
      case "r":
        if (measurementOpen && resolvedPhase(store.phase)) {
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

  // Save a deliberate resize even if the page unloads before the debounced save.
  function saveDockWidths() {
    if (resizedDock)
      savePersisted({
        ...loadPersisted(),
        dockWidth: $state.snapshot(store.dockWidth),
      });
  }
  function onNavigate() {
    const next = panelsForLayout(parseRoute(window.location.hash));
    if (serializeRoute(next) !== window.location.hash)
      window.history.replaceState(
        { graphiteRoute: false },
        "",
        serializeRoute(next),
      );
    commitRoute(next, true);
  }

  onMount(() => {
    onNavigate();
    if (authEnabled)
      void import("./AccountControl.svelte")
        .then((m) => (AccountControl = m.default))
        .catch(() => {
          // Offline lazy chunk: the meter works without the account control.
          AccountControl = null;
        });
  });
</script>

<svelte:window
  onpagehide={saveDockWidths}
  onpopstate={onNavigate}
  onkeydown={onKeydown}
  onbeforeunload={onBeforeUnload}
/>

<main
  id="console"
  {@attach observeWidth((width) => (consoleWidth = width))}
  data-phase={store.phase}
  style="--dock-left: {docks.left}px; --dock-right: {docks.right}px;"
>
  <!-- Container queries move direct actions into More as the bar narrows. -->
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
        togglePanel("settings", event.currentTarget as HTMLElement)}
      ><Icon name="settings" /></button
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
        <span class="run-icon"><Icon name={awayRunIndicator.icon} /></span>
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
        use:tooltip={historyOpen
          ? "Close History"
          : "History — saved results (H)"}
        onclick={(event) =>
          toggleHistoryFromPointer(event.currentTarget as HTMLElement)}
        ><Icon name="history" /></button
      >{/if}
    <button
      class="btn btn-icon direct-theme"
      aria-label={`Theme: ${THEME[store.theme].label}`}
      use:tooltip={`Theme: ${THEME[store.theme].label} (T) — cycles light / dark / auto`}
      onclick={toggleTheme}><Icon name={THEME[store.theme].icon} /></button
    >
    <button
      class="btn btn-icon direct-endpoint"
      aria-label="Details"
      aria-expanded={telemetryOpen}
      use:tooltip={"Details — server and connection (D)"}
      onclick={(event) =>
        togglePanel("endpoint", event.currentTarget as HTMLElement)}
      ><Icon name="info" /></button
    >
    <div class="topbar-more">
      <TopbarMore
        showHistory={store.savingResults}
        historyActive={historyOpen}
        endpointActive={telemetryOpen}
        theme={store.theme}
        onHistory={toggleHistoryFromPointer}
        onEndpoint={(invoker: HTMLElement) => togglePanel("endpoint", invoker)}
        onTheme={toggleTheme}
      />
    </div>
  </header>

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
        />{:else if historyChunkFailed}<div
          class="empty-state"
          data-tone="err"
          role="alert"
        >
          <span class="empty-icon">!</span>
          <p>History could not be opened.</p>
          <!-- Chromium keeps a failed module in its module map until the page reloads. -->
          <button
            class="btn btn-accent"
            type="button"
            onclick={() => location.reload()}>Retry</button
          >
        </div>{:else}<div class="empty-state" role="status">
          <span class="empty-icon"><Icon name="history" /></span>Opening
          History…
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

  <!-- Panels dock on wide screens and overlay below; docked widths persist. -->
  <SidePanel
    open={settingsOpen}
    docked={dockQuery.current}
    dockWidth={docks.left}
    dockMaxWidth={dockMaxLeft}
    onResize={(px) => setDockWidth("left", px)}
    onResetWidth={() => resetDockWidth("left")}
    onClose={() => dismissPanel("settings")}
    side="left"
    title="Settings"
    kicker="Test & Display"
  >
    <TestSetupPanel onOpenHistory={(invoker) => historyRoute(null, invoker)} />
  </SidePanel>
  <SidePanel
    open={telemetryOpen}
    docked={dockQuery.current}
    dockWidth={docks.right}
    dockMaxWidth={dockMaxRight}
    onResize={(px) => setDockWidth("right", px)}
    onResetWidth={() => resetDockWidth("right")}
    onClose={() => dismissPanel("endpoint")}
    title="Details"
    kicker="Server & connection"
  >
    <EndpointInfo onOpenLegal={openLegal} />
  </SidePanel>

  <PhaseToast />
  <div class="sr-only" aria-live="polite">
    {#each announcements as { id, text } (id)}<p>{text}</p>{/each}
  </div>

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
  /* Dock columns stay 0 until a docked panel fills them via display: contents. */
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
  .brand-btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: var(--space-1) 6px;
    margin-left: -6px;
    border-radius: var(--r-chrome);
    font: var(--w-heavy) var(--type-md) / 1.4 var(--font-mono);
    letter-spacing: var(--track-tight);
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
    font-weight: var(--w-heavy);
  }
  /* 44px coarse targets fit three direct actions to ~320px, one to ~260px. */
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
  /* A viewport too short for the instruments scrolls this column. */
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
      gap: var(--space-1);
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
