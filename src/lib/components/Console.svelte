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
  import TriagePanel from "./TriagePanel.svelte";
  import ReactorStage from "./ReactorStage.svelte";
  import TimeseriesTheatre from "./TimeseriesTheatre.svelte";
  import MetricChips from "./MetricChips.svelte";
  import InfraCard from "./InfraCard.svelte";
  import TelemetryDetail from "./TelemetryDetail.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import StatusBar from "./StatusBar.svelte";

  // Layout state. `inspectorVisible` drives the column (wide) AND the
  // drawer (narrow); matchMedia sets a sensible default per breakpoint.
  let inspectorVisible = $state(true);
  let railOpen = $state(true);

  function toggleTheme() {
    const cur = document.documentElement.getAttribute("data-theme");
    document.documentElement.setAttribute("data-theme", cur === "light" ? "dark" : "light");
  }

  function cycleUnit() {
    store.unitBase = store.unitBase === "base10" ? "base2" : "base10";
  }

  onMount(() => {
    const mq = window.matchMedia("(min-width: 1440px)");
    const apply = () => {
      inspectorVisible = mq.matches; // open on wide, closed (drawer) on narrow
    };
    apply();
    mq.addEventListener("change", apply);

    // Rail default per breakpoint: a grid column ≥760px, an off-canvas drawer
    // below. Crossing the boundary resets it so the rail is never stranded,
    // and ☰ always has a visible effect (collapse on wide, open drawer on narrow).
    const mqRail = window.matchMedia("(min-width: 760px)");
    const applyRail = () => {
      railOpen = mqRail.matches;
    };
    applyRail();
    mqRail.addEventListener("change", applyRail);

    void bootRunner();

    return () => {
      mq.removeEventListener("change", apply);
      mqRail.removeEventListener("change", applyRail);
      teardownRunner();
    };
  });
</script>

<main
  id="console"
  data-phase={store.phase}
  class:inspector-collapsed={!inspectorVisible}
  class:inspector-open={inspectorVisible}
  class:rail-open={railOpen}
  class:rail-collapsed={!railOpen}
  class="bg-bg text-text"
>
  <!-- TOPBAR -->
  <header class="zone topbar flex items-center gap-3 px-4 border-b border-border">
    <button
      class="ghost-btn"
      aria-label="Toggle rail"
      onclick={() => (railOpen = !railOpen)}>☰</button
    >
    <span class="font-mono text-sm font-bold tracking-tight">Graphite&nbsp;Meter</span>
    <span class="pill">{store.effectiveConnectivity}</span>
    <div class="flex-1"></div>
    <button class="ghost-btn font-mono" onclick={cycleUnit}>{store.unitBase}</button>
    <button class="ghost-btn" aria-label="Toggle theme" onclick={toggleTheme}>◐</button>
    <button
      class="ghost-btn"
      aria-label="Toggle inspector"
      onclick={() => (inspectorVisible = !inspectorVisible)}>⚙</button
    >
  </header>

  <!-- LEFT RAIL -->
  <nav class="zone rail border-r border-border bg-surface-1 p-3">
    <TriagePanel />
  </nav>

  <!-- Rail drawer backdrop (narrow viewports only) -->
  <button
    class="rail-backdrop"
    aria-label="Close rail"
    tabindex={railOpen ? 0 : -1}
    onclick={() => (railOpen = false)}
  ></button>

  <!-- CENTER STAGE -->
  <section class="zone stage min-w-0 overflow-y-auto p-4 flex flex-col gap-4">
    <ReactorStage />
    <TimeseriesTheatre />
    <MetricChips />
  </section>

  <!-- RIGHT INSPECTOR -->
  <aside class="zone inspector border-l border-border bg-surface-1 overflow-y-auto p-4 flex flex-col gap-4">
    <InfraCard />
    <LatencyProfile />
    <TelemetryDetail />
  </aside>

  <!-- Drawer backdrop (narrow viewports only) -->
  <button
    class="backdrop"
    aria-label="Close inspector"
    tabindex={inspectorVisible ? 0 : -1}
    onclick={() => (inspectorVisible = false)}
  ></button>

  <!-- STATUS BAR -->
  <footer class="zone status flex items-center gap-4 px-4 border-t border-border bg-surface-1 font-mono text-soft">
    <StatusBar />
  </footer>
</main>

<style>
  /* ===== Three-zone grid (§1.1) ===== */
  #console {
    display: grid;
    grid-template-columns: var(--rail-w) minmax(0, 1fr) var(--inspector-w);
    grid-template-rows: var(--topbar-h) minmax(0, 1fr) 28px;
    grid-template-areas:
      "topbar  topbar  topbar"
      "rail    stage   inspector"
      "status  status  status";
    height: 100dvh;
    gap: 0;
  }

  .topbar {
    grid-area: topbar;
  }
  .rail {
    grid-area: rail;
    overflow: hidden auto; /* vertical scroll for tall rails */
  }
  .stage {
    grid-area: stage;
  }
  .inspector {
    grid-area: inspector;
  }
  .status {
    grid-area: status;
    font-size: 11px;
  }

  /* Manual collapse (grid mode, ≥760): zero the track AND remove the rail
     from the render tree, so no content can peek behind the stage panel. */
  #console.inspector-collapsed {
    --inspector-w: 0px;
  }
  #console.rail-collapsed {
    --rail-w: 0px;
  }
  #console.rail-collapsed .rail {
    display: none;
  }
  /* Same as the rail: when collapsed in grid mode (≥1440) drop the inspector
     from the render tree so its 0-width column can't overflow to the right. */
  #console.inspector-collapsed .inspector {
    display: none;
  }
  .inspector {
    overflow: hidden auto;
  }

  /* Drawer backdrops are inert on wide layouts */
  .backdrop,
  .rail-backdrop {
    display: none;
    border: 0;
    padding: 0;
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

  .pill {
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 11px;
  }

  /* ===== Responsive collapse (§1.1) ===== */

  /* 1100–1439: inspector becomes a right slide-in overlay drawer */
  @media (max-width: 1439px) {
    #console {
      grid-template-columns: var(--rail-w) minmax(0, 1fr);
      grid-template-areas:
        "topbar  topbar"
        "rail    stage"
        "status  status";
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

  /* (The former 760–1099 56px icon-strip half-state was removed: the rail is
     now binary — full column or fully hidden — which is cleaner and avoids
     clipped, unusable icon-only content.) */

  /* < 760: single-column stack; the rail becomes a left slide-in drawer
     (mirroring the inspector) so it stays reachable via ☰. */
  @media (max-width: 759px) {
    #console {
      grid-template-columns: 1fr;
      grid-template-areas:
        "topbar"
        "stage"
        "status";
      height: auto;
      min-height: 100dvh;
    }
    .rail {
      position: fixed;
      top: var(--topbar-h);
      left: 0;
      bottom: 28px;
      width: min(280px, 82vw);
      transform: translateX(-100%);
      transition: transform var(--dur-slide) var(--ease-out);
      z-index: 40;
      box-shadow: var(--shadow-float);
    }
    /* In drawer mode hide via transform (animatable), overriding the
       grid-mode display:none so the slide transition can play. */
    #console.rail-collapsed .rail {
      display: block;
    }
    #console.rail-open .rail {
      transform: translateX(0);
    }
    #console.rail-open .rail-backdrop {
      display: block;
      position: fixed;
      inset: var(--topbar-h) 0 28px 0;
      z-index: 30;
      background: color-mix(in srgb, var(--canvas) 50%, transparent);
    }
  }
</style>
