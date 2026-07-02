<script lang="ts">
  /* ============================================================
   * <Console> — the Control Console shell (§1.1 + §4.2)
   * A single gauge-first stage between the topbar and status bar.
   * Two auxiliary surfaces — Settings (left) and the
   * Connection & telemetry inspector (right) — are identical flyout
   * panels built on the shared <SidePanel>; both are closed by default
   * so the default view stays focused on the instrument (§14.2).
   * ============================================================ */
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { bootRunner, teardownRunner } from "../runner/wire.svelte";
  import GaugePanel from "./GaugePanel.svelte";
  import ThroughputChart from "./ThroughputChart.svelte";
  import StatusBar from "./StatusBar.svelte";
  import SettingsPanel from "./settings/SettingsPanel.svelte";
  import TelemetryPanel from "./TelemetryPanel.svelte";
  import PhaseToast from "./PhaseToast.svelte";
  import ShortcutHints from "./ShortcutHints.svelte";
  import ConnectivityIndicator from "./ConnectivityIndicator.svelte";
  import { engage, returnToStart } from "../runner/wire.svelte";
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";
  import { mediaQuery } from "../actions/mediaQuery.svelte";
  import { DEFAULT_DOCK_WIDTH } from "../state/persistence";

  // The two auxiliary panels. Both closed by default (progressive disclosure):
  // the gauge owns the default view; Settings/telemetry are summoned from the
  // topbar or via the W / D shortcuts. On wide screens an open panel docks as
  // an in-flow column that pushes the stage; below that it's a flyout overlay.
  // Same shared <SidePanel> either way.
  let telemetryOpen = $state(false);
  let settingsOpen = $state(false);
  const dockQuery = mediaQuery(`(min-width: 1200px)`); // bp: dock

  // Track which drawer opened most recently: it stacks on top when both are
  // docked, and in flyout mode (not docked) opening one closes the other —
  // overlapping flyouts/bottom sheets would bury whichever is beneath.
  // Captures any open transition regardless of source (button, W/D key, bind).
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

  function toggleTheme() {
    // Flip the persisted pref; the store's $effect applies it to
    // <html data-theme> and the debounced save persists it (§14.1).
    store.theme = store.theme === "light" ? "dark" : "light";
  }

  // Docked-panel widths (persisted). The reserved grid column is the saved
  // width only while that panel is docked + open, else 0 (column collapses).
  // The grid template also CSS-clamps to 46vw so a stale large value is safe.
  const dockLeft = $derived(dockQuery.matches && settingsOpen ? store.dockWidth.left : 0);
  const dockRight = $derived(dockQuery.matches && telemetryOpen ? store.dockWidth.right : 0);

  function setDockWidth(side: "left" | "right", px: number) {
    store.dockWidth = { ...store.dockWidth, [side]: px };
  }
  function resetDockWidth(side: "left" | "right") {
    store.dockWidth = { ...store.dockWidth, [side]: DEFAULT_DOCK_WIDTH[side] };
  }

  /* ---- Global keyboard map (§7, extended by Batch G §13.7) ----
     | Key            | Action                                          |
     | Space / Enter  | Engage / Abort (wire.engage toggle)             |
     | Esc            | Abort if running, else close any open panel     |
     | W              | Toggle Settings                                    |
     | D              | Toggle Connection & telemetry                   |
     | R              | Re-run when phase is complete                   |
     | T              | Cycle theme                                     |
     Guarded against text inputs / contentEditable so typing is never hijacked. */
  function inEditable(el: EventTarget | null): boolean {
    if (!(el instanceof HTMLElement)) return false;
    const tag = el.tagName;
    return (
      tag === "INPUT" ||
      tag === "TEXTAREA" ||
      tag === "SELECT" ||
      el.isContentEditable
    );
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
    if (inEditable(e.target)) return;

    // Esc aborts a live run, otherwise closes whichever panel is open.
    if (e.key === "Escape") {
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
      // If a button/link is focused, let its native activation fire instead so
      // we never double-toggle (e.g. Enter on the focused RunButton).
      const t = e.target;
      if (t instanceof HTMLElement) {
        const tag = t.tagName;
        if (tag === "BUTTON" || tag === "A" || t.getAttribute("role") === "button") return;
      }
      engage();
      e.preventDefault();
      return;
    }

    switch (e.key.toLowerCase()) {
      case "w":
        settingsOpen = !settingsOpen;
        e.preventDefault();
        break;
      case "d":
        telemetryOpen = !telemetryOpen;
        e.preventDefault();
        break;
      case "r":
        if (store.phase === "complete") {
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
    void bootRunner();

    return () => {
      window.removeEventListener("keydown", onKeydown);
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
  <header class="zone topbar flex items-center gap-3 px-4 border-b border-border">
    <button
      type="button"
      class="brand-btn font-mono text-sm font-bold tracking-tight"
      aria-label="Graphite Meter — return to a fresh, blank test"
      use:tooltip={"Return to a fresh, blank test"}
      onclick={returnToStart}
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
      >Graphite&nbsp;Meter</button
    >
    <button
      class="ghost-btn icon-btn"
      aria-label="Open settings"
      aria-expanded={settingsOpen}
      use:tooltip={"Settings — setup, endpoint, developer (W)"}
      onclick={() => (settingsOpen = !settingsOpen)}>{@html ICON.settings}</button
    >
    <ConnectivityIndicator />
    <div class="flex-1"></div>
    <button
      class="ghost-btn"
      aria-label="Toggle light or dark theme"
      use:tooltip={"Toggle light / dark theme (T)"}
      onclick={toggleTheme}>◐</button
    >
    <button
      class="ghost-btn icon-btn"
      aria-label="Toggle telemetry"
      aria-expanded={telemetryOpen}
      use:tooltip={"Telemetry (D)"}
      onclick={() => (telemetryOpen = !telemetryOpen)}>{@html ICON.info}</button
    >
  </header>

  <!-- CENTER STAGE — height-bounded flex column (§14.2). The gauge hero is the
       focal point and takes the lion's share; the chart is secondary and
       compact so the simple default fits the viewport without vertical scroll.
       Results now live INSIDE GaugePanel (the instrument cluster morphs
       between live/partial-results/final-grid states) rather than as a
       separate flex sibling here, so toggling a stage or advancing a phase
       never resizes anything in this section. -->
  <section class="zone stage min-w-0 overflow-y-auto flex flex-col">
    <GaugePanel />
    <ThroughputChart />
  </section>

  <!-- STATUS BAR -->
  <footer class="zone status flex items-center gap-4 px-4 border-t border-border bg-surface-1 font-mono text-soft">
    <StatusBar />
    <ShortcutHints />
  </footer>

  <!-- Auxiliary panels — identical shared base, opposite sides; dock on wide
       screens (pushing the stage), flyout overlay below that. Docked panels are
       resizable from their inner edge (persisted via store.dockWidth). -->
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

  <!-- Transient phase-change toast (§13.7) — fixed, bottom-right -->
  <PhaseToast />
</main>

<style>
  /* ===== Console grid (§1.1) =====
     The stage owns the middle; the left/right dock columns are 0-width until a
     panel docks (wide screens), at which point the matching .dock-* class
     reserves space and the panel (a <SidePanel> grid child via display:contents)
     slots into leftdock / rightdock, pushing the stage. Below the dock
     breakpoint the panels are flyout overlays and these columns stay collapsed. */
  #console {
    display: grid;
    /* Dock columns are driven by inline --dock-left/right (0 when not
       docked-open) and CSS-clamped to 46vw so a stale/large saved width can
       never starve the stage. */
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
    /* Height-bounded flex column: the hero gauge flexes to fill, chart + chips
       stay at their compact intrinsic height. The faceplate surface; the wells
       inside it carry the depth. Token-driven spacing (density knob). */
    padding: var(--space-3);
    gap: var(--space-3);
    /* Keep stage scrolling from chaining out to the document (anchored bars). */
    overscroll-behavior: contain;
  }
  /* The gauge hero is the focal point — it absorbs spare height (flex-grow) on
     tall screens. Crucially it must NOT shrink below its own content: with
     min-height:0 the gauge panel would under-shrink and its gauge/controls would
     overflow and overlap the chart/chips. Keeping the content floor means the
     stage column overflows and the stage (overflow-y:auto) scrolls instead —
     correct when space is tight (e.g. both panels docked on a narrow stage). */
  .stage > :global(.gauge-panel) {
    flex: 1 1 auto;
  }
  .stage > :global(.chart) {
    flex: 0 0 auto;
  }
  .status {
    grid-area: status;
    font-size: 11px;
    /* Fixed-height chrome strip: clip anything that can't fit rather than let
       text spill out or wrap past the 28px row at awkward widths. */
    min-width: 0;
    overflow: hidden;
  }

  /* The logo doubles as a "home" action — reads as the wordmark, with just a
     hover/focus affordance to signal it's clickable. */
  .brand-btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 4px 6px;
    margin-left: -6px;
    border: 0;
    border-radius: var(--radius-sm);
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
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
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

  /* < 760: the whole document scrolls instead of the stage. The stage must
     stop being an overscroll-containing scroll container here — otherwise it
     captures wheel/touch gestures over the middle and, having nothing to
     scroll internally, refuses to chain them out to the document (the page
     becomes unscrollable from the center, breaking phone usability). Letting
     it overflow visibly returns scroll control to the document. */
  @media (max-width: 759px) { /* bp: stacked */
    #console {
      height: auto;
      min-height: 100dvh;
    }
    .stage {
      overflow-y: visible;
      overscroll-behavior: auto;
    }
  }
</style>
