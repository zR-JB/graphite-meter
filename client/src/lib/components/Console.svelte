<script lang="ts">
  // Main console shell: boots the runner, owns top-level panels, shortcuts,
  // theme toggle, and docked/flyout layout state.
  import { onMount, type Component } from "svelte";
  import { store } from "../state/store.svelte";
  import { bootRunner, teardownRunner } from "../runner/engine.svelte";
  import GaugePanel from "./GaugePanel.svelte";
  import ThroughputChart from "./ThroughputChart.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./settings/SettingsPanel.svelte";
  import TelemetryPanel from "./TelemetryPanel.svelte";
  import PhaseToast from "./PhaseToast.svelte";
  import ShortcutHints from "./ShortcutHints.svelte";
  import ConnectivityIndicator from "./ConnectivityIndicator.svelte";
  import { engage, returnToStart } from "../runner/engine.svelte";
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";
  import { focusTrap } from "../actions/focusTrap";
  import { mediaQuery } from "../actions/mediaQuery.svelte";
  import { DEFAULT_DOCK_WIDTH } from "../state/persistence";
  import { authEnabled } from "../auth";

  let AccountControl = $state<Component | null>(null);

  let telemetryOpen = $state(false);
  let settingsOpen = $state(false);
  let resetConfirmOpen = $state(false);
  const dockQuery = mediaQuery(`(min-width: 1200px)`);
  const RESOLVED_PHASES = ["complete", "aborted", "error"];

  // In flyout mode the auxiliary panels are mutually exclusive. On docked
  // layouts both may be open, and the most recently opened panel stacks above.
  let lastOpened = $state<"left" | "right">("right");
  let prevSettingsOpen = false;
  let prevTelemetryOpen = false;
  $effect(() => {
    if (settingsOpen && !prevSettingsOpen) {
      lastOpened = "left";
      if (!dockQuery.matches) telemetryOpen = false;
    }
    if (telemetryOpen && !prevTelemetryOpen) {
      lastOpened = "right";
      if (!dockQuery.matches) settingsOpen = false;
    }
    prevSettingsOpen = settingsOpen;
    prevTelemetryOpen = telemetryOpen;
  });

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

  // Persisted widths become reserved grid columns only while that side is
  // actually docked and open; otherwise the stage gets the space back.
  const dockLeft = $derived(
    dockQuery.matches && settingsOpen ? store.dockWidth.left : 0,
  );
  const dockRight = $derived(
    dockQuery.matches && telemetryOpen ? store.dockWidth.right : 0,
  );

  function setDockWidth(side: "left" | "right", px: number) {
    store.dockWidth = { ...store.dockWidth, [side]: px };
  }
  function resetDockWidth(side: "left" | "right") {
    store.dockWidth = { ...store.dockWidth, [side]: DEFAULT_DOCK_WIDTH[side] };
  }

  function requestReturnToStart() {
    if (store.isRunning) {
      resetConfirmOpen = true;
      return;
    }
    returnToStart();
  }

  function confirmReturnToStart() {
    resetConfirmOpen = false;
    returnToStart();
  }

  function onBeforeUnload(e: BeforeUnloadEvent) {
    if (!store.isRunning) return;
    e.preventDefault();
    e.returnValue = "";
  }

  function onAuthRequired() {
    teardownRunner();
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
    if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
    if (isEditable(e.target)) return;
    if (resetConfirmOpen) return;

    if (e.key === "Escape") {
      // Esc aborts a live run first; otherwise it closes the visible panel.
      if (store.isRunning) {
        engage();
      } else if (settingsOpen) {
        settingsOpen = false;
      } else if (telemetryOpen) {
        telemetryOpen = false;
      } else {
        return;
      }
      e.preventDefault();
      return;
    }

    if (e.key === " " || e.key === "Enter") {
      if (selfActivating(e.target)) return;
      engage();
      e.preventDefault();
      return;
    }

    switch (e.key.toLowerCase()) {
      case "s":
        settingsOpen = !settingsOpen;
        e.preventDefault();
        break;
      case "d":
        telemetryOpen = !telemetryOpen;
        e.preventDefault();
        break;
      case "r":
        if (RESOLVED_PHASES.includes(store.phase)) {
          engage();
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
    window.addEventListener("keydown", onKeydown);
    window.addEventListener("beforeunload", onBeforeUnload);
    window.addEventListener("graphite-meter-auth-required", onAuthRequired);
    void bootRunner();
    if (authEnabled)
      void import("./AccountControl.svelte").then(
        (m) => (AccountControl = m.default),
      );

    return () => {
      window.removeEventListener("keydown", onKeydown);
      window.removeEventListener("beforeunload", onBeforeUnload);
      window.removeEventListener(
        "graphite-meter-auth-required",
        onAuthRequired,
      );
      teardownRunner();
    };
  });
</script>

<main
  id="console"
  data-phase={store.phase}
  style="--dock-left: {dockLeft}px; --dock-right: {dockRight}px;"
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
      aria-label="Graphite Meter — return to a fresh, blank test"
      use:tooltip={"Return to a fresh, blank test"}
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
      use:tooltip={"Settings — setup, endpoint, developer (S)"}
      onclick={() => (settingsOpen = !settingsOpen)}
      >{@html ICON.settings}</button
    >
    <ConnectivityIndicator />
    <div class="flex-1"></div>
    {#if AccountControl}<AccountControl />{/if}
    <button
      class="ghost-btn icon-btn"
      aria-label={`Theme: ${THEME_LABEL[store.theme]}. Click to cycle light / dark / auto.`}
      use:tooltip={`Theme: ${THEME_LABEL[store.theme]} (T) — cycles light / dark / auto`}
      onclick={toggleTheme}>{@html THEME_ICON[store.theme]}</button
    >
    <button
      class="ghost-btn icon-btn"
      aria-label="Toggle endpoint info"
      aria-expanded={telemetryOpen}
      use:tooltip={"Endpoint info"}
      onclick={() => (telemetryOpen = !telemetryOpen)}>{@html ICON.info}</button
    >
  </header>

  <!-- Center stage: a height-bounded flex column. The gauge hero takes most of
       it, the chart stays compact so the default fits the viewport without
       vertical scroll. Advancing a phase never resizes this section. -->
  <section class="zone stage flex min-w-0 flex-col overflow-y-auto">
    <GaugePanel />
    <ThroughputChart />
  </section>

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
    bind:open={settingsOpen}
    docked={dockQuery.matches}
    raised={lastOpened === "left"}
    dockWidth={store.dockWidth.left}
    onResize={(px) => setDockWidth("left", px)}
    onResetWidth={() => resetDockWidth("left")}
  />
  <TelemetryPanel
    bind:open={telemetryOpen}
    docked={dockQuery.matches}
    raised={lastOpened === "right"}
    dockWidth={store.dockWidth.right}
    onResize={(px) => setDockWidth("right", px)}
    onResetWidth={() => resetDockWidth("right")}
  />

  <!-- Transient phase-change toast, pinned bottom-right. -->
  <PhaseToast />

  {#if resetConfirmOpen}
    <div class="confirm-backdrop">
      <div
        class="confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="reset-confirm-title"
        aria-describedby="reset-confirm-copy"
        tabindex="-1"
        use:focusTrap={true}
        onkeydown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            resetConfirmOpen = false;
          }
        }}
      >
        <h2 id="reset-confirm-title">Stop the running test?</h2>
        <p id="reset-confirm-copy">
          Returning to a fresh test will abort the measurement in progress.
        </p>
        <div class="confirm-actions">
          <button
            class="ghost-btn"
            type="button"
            onclick={() => (resetConfirmOpen = false)}
          >
            Keep running
          </button>
          <button
            class="danger-btn"
            type="button"
            onclick={confirmReturnToStart}
          >
            Stop test
          </button>
        </div>
      </div>
    </div>
  {/if}
</main>

<style>
  /* Console grid: the stage owns the middle, the dock columns are 0-width
     until a panel docks on a wide screen. A docked <SidePanel> reaches
     leftdock/rightdock through display:contents and pushes the stage. Below
     the dock breakpoint the panels are flyout overlays. */
  #console {
    display: grid;
    /* Inline --dock-left/right drive these, 0 when not docked-open. The 46vw
       clamp stops a stale saved width from starving the stage. */
    grid-template-columns:
      min(var(--dock-left, 0px), 46vw)
      minmax(0, 1fr)
      min(var(--dock-right, 0px), 46vw);
    grid-template-rows: var(--topbar-h) minmax(0, 1fr) 28px;
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
    padding: var(--space-3);
    gap: var(--space-3);
    /* Keep stage scrolling from chaining out to the document (anchored bars). */
    overscroll-behavior: contain;
  }
  /* Spare height splits gauge-first: 3 shares to the gauge, 1 to the chart.
     The auto basis is a content floor: below it the gauge controls overlap the
     chart, so the stage column overflows and scrolls instead. */
  .stage > :global(.gauge-panel) {
    flex: 3 1 auto;
    min-height: 0;
  }
  /* The chart takes leftover height, capped so a tall viewport returns the
     excess to the gauge. Its ResizeObserver re-rasterizes the canvas, and its
     flex-basis is the 140px plot floor. */
  .stage > :global(.chart) {
    flex: 1 1 140px;
    min-height: 0;
    max-height: 340px;
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

  .confirm-backdrop {
    position: fixed;
    inset: 0;
    z-index: 180;
    display: grid;
    place-items: center;
    padding: var(--space-4);
    background: color-mix(in srgb, var(--canvas) 64%, transparent);
  }
  .confirm-dialog {
    width: min(360px, 100%);
    padding: var(--space-4);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--shadow-float);
  }
  .confirm-dialog h2 {
    margin: 0;
    font-family: var(--font-display);
    font-size: var(--type-lg);
    font-weight: 650;
    letter-spacing: 0;
  }
  .confirm-dialog p {
    margin: var(--space-2) 0 0;
    color: var(--text-muted);
    font-size: var(--type-sm);
    line-height: 1.45;
  }
  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    margin-top: var(--space-4);
  }
  .danger-btn {
    display: grid;
    place-items: center;
    height: 32px;
    padding: 0 var(--space-3);
    border: 1px solid color-mix(in srgb, var(--err) 55%, var(--border));
    border-radius: var(--r-chrome);
    background: var(--err-soft);
    color: var(--text);
    font-size: var(--type-sm);
    font-weight: 650;
  }
  .danger-btn:hover {
    border-color: var(--err);
  }

  /* Under 760px the document scrolls, not the stage. An overscroll-containing
     stage swallows wheel and touch gestures over its middle. Visible overflow
     returns them to the page. */
  @media (max-width: 759px) {
    /* bp: stacked */
    #console {
      height: auto;
      min-height: 100dvh;
    }
    .stage {
      overflow-y: visible;
      overscroll-behavior: auto;
    }
  }
  @media (max-width: 520px) {
    .topbar.authenticated .brand-label {
      display: none;
    }
  }
</style>
