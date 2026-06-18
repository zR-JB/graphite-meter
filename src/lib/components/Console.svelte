<script lang="ts">
  /* ============================================================
   * <Console> — the Control Console shell (§1.1 + §4.2)
   * Three-zone grid with responsive collapse. Zones currently
   * hold labelled placeholder boxes; real components land in the
   * next stage (§5 step 6+).
   * ============================================================ */
  import { onMount } from "svelte";
  import { console as store } from "../state/console.svelte";
  import { bootRunner, teardownRunner } from "../runner/wire.svelte";
  import ReactorStage from "./ReactorStage.svelte";
  import TimeseriesTheatre from "./TimeseriesTheatre.svelte";
  import MetricChips from "./MetricChips.svelte";
  import InfraCard from "./InfraCard.svelte";
  import TelemetryDetail from "./TelemetryDetail.svelte";
  import StatusBar from "./StatusBar.svelte";
  import WorkbenchDrawer from "./workbench/WorkbenchDrawer.svelte";
  import PhaseToast from "./PhaseToast.svelte";
  import CommandHints from "./CommandHints.svelte";
  import ConnectivityPulse from "./ConnectivityPulse.svelte";
  import { engage, returnToStart } from "../runner/wire.svelte";
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";

  // Layout state. `inspectorVisible` drives the column (wide) AND the
  // drawer (narrow); matchMedia sets a sensible default per breakpoint.
  // The left rail is dissolved (§14.2): primary controls (stage chips +
  // Engage) now live in the hero stage, advanced config in the Workbench.
  let inspectorVisible = $state(true);
  // The Workbench power-user drawer (§13.6). Keyboard shortcut `W` lands in
  // Batch G; for now the topbar flask button is the only trigger.
  let workbenchOpen = $state(false);

  function toggleTheme() {
    // Flip the persisted pref; the store's $effect applies it to
    // <html data-theme> and the debounced save persists it (§14.1).
    store.theme = store.theme === "light" ? "dark" : "light";
  }

  /* ---- Global keyboard map (§7, extended by Batch G §13.7) ----
     | Key            | Action                                          |
     | Space / Enter  | Engage / Abort (wire.engage toggle)             |
     | Esc            | Abort if running, else close any open drawer    |
     | W              | Toggle Workbench                                |
     | D              | Toggle inspector (Details)                      |
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

    // Esc is allowed even mid-typing-context above already returned; here it
    // aborts a live run, otherwise closes whichever drawer is open.
    if (e.key === "Escape") {
      if (store.isRunning) {
        engage();
      } else if (workbenchOpen) {
        workbenchOpen = false;
      } else if (inspectorVisible) {
        inspectorVisible = false;
      } else {
        return;
      }
      e.preventDefault();
      return;
    }

    if (e.key === " " || e.key === "Enter") {
      // If a button/link is focused, let its native activation fire instead so
      // we never double-toggle (e.g. Enter on the focused EngageButton).
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
        workbenchOpen = !workbenchOpen;
        e.preventDefault();
        break;
      case "d":
        inspectorVisible = !inspectorVisible;
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
    const mq = window.matchMedia("(min-width: 1440px)");
    const apply = () => {
      inspectorVisible = mq.matches; // open on wide, closed (drawer) on narrow
    };
    apply();
    mq.addEventListener("change", apply);

    window.addEventListener("keydown", onKeydown);

    void bootRunner();

    return () => {
      mq.removeEventListener("change", apply);
      window.removeEventListener("keydown", onKeydown);
      teardownRunner();
    };
  });
</script>

<main
  id="console"
  data-phase={store.phase}
  class:inspector-collapsed={!inspectorVisible}
  class:inspector-open={inspectorVisible}
  class="bg-bg text-text"
>
  <!-- TOPBAR -->
  <header class="zone topbar flex items-center gap-3 px-4 border-b border-border">
    <button
      type="button"
      class="brand-btn font-mono text-sm font-bold tracking-tight"
      aria-label="Graphite Meter — return to a fresh, blank test"
      use:tooltip={"Return to a fresh, blank test"}
      onclick={returnToStart}>Graphite&nbsp;Meter</button
    >
    <button
      class="ghost-btn icon-btn"
      aria-label="Open settings workbench"
      aria-expanded={workbenchOpen}
      use:tooltip={"Settings — test setup, infrastructure, developer (W)"}
      onclick={() => (workbenchOpen = !workbenchOpen)}>{@html ICON.settings}</button
    >
    <ConnectivityPulse />
    <div class="flex-1"></div>
    <button
      class="ghost-btn"
      aria-label="Toggle light or dark theme"
      use:tooltip={"Toggle light / dark theme (T)"}
      onclick={toggleTheme}>◐</button
    >
    <button
      class="ghost-btn icon-btn"
      aria-label="Toggle connection & telemetry info"
      aria-expanded={inspectorVisible}
      use:tooltip={"Connection & telemetry info (D)"}
      onclick={() => (inspectorVisible = !inspectorVisible)}>{@html ICON.info}</button
    >
  </header>

  <!-- CENTER STAGE — height-bounded flex column (§14.2). The gauge hero is the
       focal point and takes the lion's share; chart + chips are secondary and
       compact so the simple default fits the viewport without vertical scroll. -->
  <section class="zone stage min-w-0 overflow-y-auto flex flex-col">
    <ReactorStage />
    <TimeseriesTheatre />
    <MetricChips />
  </section>

  <!-- RIGHT INSPECTOR — InfraCard (connection) only in the default view. The
       latency profile moved into the gauge card as a compact strip below the
       gauge (§14.2), so it reads with the instrument. TelemetryDetail's heavy
       percentiles/jitter stay secondary: tucked behind an opt-in disclosure so
       the default view stays uncluttered. -->
  <aside class="zone inspector border-l border-border bg-surface-1 overflow-y-auto p-4 flex flex-col gap-4" aria-label="Connection and telemetry info">
    <InfraCard />
    <details class="telemetry-disclose">
      <summary>Show detailed telemetry</summary>
      <div class="telemetry-disclose__body">
        <TelemetryDetail />
      </div>
    </details>
  </aside>

  <!-- Drawer backdrop (narrow viewports only) -->
  <button
    class="backdrop"
    aria-label="Close info panel"
    tabindex={inspectorVisible ? 0 : -1}
    onclick={() => (inspectorVisible = false)}
  ></button>

  <!-- STATUS BAR -->
  <footer class="zone status flex items-center gap-4 px-4 border-t border-border bg-surface-1 font-mono text-soft">
    <StatusBar />
    <CommandHints />
  </footer>

  <!-- Workbench power-user drawer (§13.6) — fixed overlay, opens from topbar -->
  <WorkbenchDrawer bind:open={workbenchOpen} />

  <!-- Transient phase-change toast (§13.7) — fixed, bottom-right -->
  <PhaseToast />
</main>

<style>
  /* ===== Two-zone console grid (§1.1, rail dissolved §14.2) =====
     Stage + inspector; the old left control rail is gone — primary
     controls now live in the hero stage. */
  #console {
    display: grid;
    grid-template-columns: minmax(0, 1fr) var(--inspector-w);
    grid-template-rows: var(--topbar-h) minmax(0, 1fr) 28px;
    grid-template-areas:
      "topbar  topbar"
      "stage   inspector"
      "status  status";
    height: 100dvh;
    gap: 0;
  }

  .topbar {
    grid-area: topbar;
  }
  .stage {
    grid-area: stage;
    /* Height-bounded flex column: the hero gauge flexes to fill, chart + chips
       stay at their compact intrinsic height. Tighter pad/gap than the cards so
       the simple default fits ~1280×800 and ~1440×900 without vertical scroll. */
    padding: 12px;
    gap: 12px;
  }
  /* The gauge hero is the focal point — let it absorb spare height; the chart
     and chips below it never grow past their compact basis. */
  .stage > :global(.reactor) {
    flex: 1 1 auto;
    min-height: 0;
  }
  .stage > :global(.theatre),
  .stage > :global(.chips),
  .stage > :global(.metric-guidance) {
    flex: 0 0 auto;
  }
  .inspector {
    grid-area: inspector;
  }
  .status {
    grid-area: status;
    font-size: 11px;
  }

  /* Manual collapse (grid mode, ≥1440): zero the track AND drop the inspector
     from the render tree so its 0-width column can't overflow to the right. */
  #console.inspector-collapsed {
    --inspector-w: 0px;
  }
  #console.inspector-collapsed .inspector {
    display: none;
  }
  .inspector {
    overflow: hidden auto;
  }

  /* Drawer backdrop is inert on wide layouts */
  .backdrop {
    display: none;
    border: 0;
    padding: 0;
  }

  /* The logo doubles as a "home" action — reads as the wordmark, with just a
     hover/focus affordance to signal it's clickable. */
  .brand-btn {
    padding: 4px 6px;
    margin-left: -6px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    cursor: pointer;
    transition: color var(--dur-hover) var(--ease-out);
  }
  .brand-btn:hover {
    color: var(--brand-strong);
  }
  .brand-btn:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
  }

  .ghost-btn {
    display: grid;
    place-items: center;
    min-width: 32px;
    height: 32px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    color: var(--text-muted);
    font-size: 13px;
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

  /* Opt-in disclosure wrapping the heavy telemetry (§14.2). */
  .telemetry-disclose {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-card);
    overflow: clip;
  }
  .telemetry-disclose > summary {
    list-style: none;
    cursor: pointer;
    padding: 13px 14px;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.02em;
    color: var(--text-muted);
    transition: color var(--dur-hover) var(--ease-out);
  }
  .telemetry-disclose > summary::-webkit-details-marker {
    display: none;
  }
  .telemetry-disclose > summary::before {
    content: "▸";
    display: inline-block;
    margin-right: 8px;
    color: var(--text-soft);
    transition: transform var(--dur-hover) var(--ease-out);
  }
  .telemetry-disclose[open] > summary::before {
    transform: rotate(90deg);
  }
  .telemetry-disclose > summary:hover {
    color: var(--text);
  }
  .telemetry-disclose > summary:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: -2px;
  }
  /* The nested card already has its own border/radius; drop the top border so
     it reads as one continuous panel under the summary. */
  .telemetry-disclose__body :global(.card) {
    border: 0;
    border-top: 1px solid var(--border);
    border-radius: 0;
    box-shadow: none;
  }

  /* ===== Responsive collapse (§1.1) ===== */

  /* 1100–1439: inspector becomes a right slide-in overlay drawer */
  @media (max-width: 1439px) {
    #console {
      grid-template-columns: minmax(0, 1fr);
      grid-template-areas:
        "topbar"
        "stage"
        "status";
    }
    .inspector {
      position: fixed;
      top: var(--topbar-h);
      right: 0;
      bottom: 28px;
      width: var(--inspector-w);
      transform: translateX(100%);
      transition: transform var(--dur-slide) var(--ease-out);
      z-index: 40;
      box-shadow: var(--shadow-float);
    }
    /* In drawer mode keep the inspector rendered (overriding the grid-mode
       display:none) so its slide transition can play; off-canvas content is
       clipped by the body's overflow-x:hidden. */
    #console.inspector-collapsed .inspector {
      display: block;
    }
    #console.inspector-open .inspector {
      transform: translateX(0);
    }
    #console.inspector-open .backdrop {
      display: block;
      position: fixed;
      inset: var(--topbar-h) 0 28px 0;
      z-index: 30;
      background: color-mix(in srgb, var(--canvas) 50%, transparent);
    }
  }

  /* < 760: single-column stack; the stage flows above the status bar and the
     inspector stays a right slide-in drawer (from the ≥max-1439 rule above). */
  @media (max-width: 759px) {
    #console {
      height: auto;
      min-height: 100dvh;
    }
  }
</style>
