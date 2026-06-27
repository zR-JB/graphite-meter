<script lang="ts">
  /* ============================================================
   * <ThroughputChart> — dual-axis canvas chart (§3.2)
   * Thin wrapper around ChartEngine. Owns hover scrub: feeds the
   * cursor x to the engine (which draws the guideline) and renders
   * a floating mono readout chip in DOM.
   * ============================================================ */
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { ChartEngine, type HoverInfo } from "../canvas/ChartEngine";
  import { fmtSpeed, fmtMs } from "../format";

  let canvasEl = $state<HTMLCanvasElement>();
  let engine: ChartEngine;
  let hover = $state<HoverInfo | null>(null);

  // Wake the (self-parking) chart loop whenever the data it draws changes.
  // During a run the loop sustains itself; this re-arms it on engage, on a new
  // run (runSeq), and on the latency-enabled toggle while parked.
  $effect(() => {
    void store.phase;
    void store.runSeq;
    void store.throughput.length;
    void store.latency.length;
    void store.latencyEnabled;
    engine?.wake();
  });

  function onMove(e: MouseEvent) {
    if (!engine) return;
    engine.setHover(e.offsetX);
    hover = engine.hoverInfo();
  }
  function onLeave() {
    engine?.setHover(null);
    hover = null;
  }

  onMount(() => {
    engine = new ChartEngine(
      () => ({
        throughput: store.throughput,
        latency: store.latencyPlot, // bucketed line (raw `latency` still feeds the stats)
        latencyEnabled: store.latencyEnabled,
        phase: store.phase,
        runSeq: store.runSeq,
      }),
      {
        throughput: (bytesPerSec) => fmtSpeed(store.toUnit(bytesPerSec)),
        latency: (rtt) => fmtMs(rtt),
      },
    );
    engine.attach(canvasEl!);
    engine.start();

    // invalidateTheme repaints synchronously, so theme/resize need no wake().
    const mo = new MutationObserver(() => engine.invalidateTheme());
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const ro = new ResizeObserver(() => engine.invalidateTheme());
    ro.observe(canvasEl!);

    return () => {
      engine.destroy();
      mo.disconnect();
      ro.disconnect();
    };
  });
</script>

<section class="chart">
  <div
    class="plot"
    role="img"
    aria-label="Throughput and latency over time"
    onmousemove={onMove}
    onmouseleave={onLeave}
  >
    <canvas bind:this={canvasEl} class="canvas"></canvas>

    {#if hover}
      <div
        class="chip"
        style="left:{hover.x}px"
        class:flip={canvasEl && hover.x > canvasEl.clientWidth - 130}
      >
        <div class="chip-row"><span>t</span><b>{(hover.t / 1000).toFixed(1)}s</b></div>
        {#if hover.bytesPerSec != null}
          <div class="chip-row">
            <span>rate</span><b>{fmtSpeed(store.toUnit(hover.bytesPerSec))} {store.unitLabel}</b>
          </div>
        {/if}
        {#if hover.rtt != null}
          <div class="chip-row"><span>rtt</span><b>{fmtMs(hover.rtt)} ms</b></div>
        {/if}
      </div>
    {/if}
  </div>
</section>

<style>
  /* Flat milled tile on the faceplate (quieter than the gauge well). */
  .chart {
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  /* Secondary, compact: a modest capped height keeps the chart legible while
     leaving the gauge hero as the focal point and the stage scroll-free. The
     plot screen is a shallow recess set into the tile. */
  .plot {
    position: relative;
    height: 140px;
    border-radius: var(--r-well);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
    overflow: hidden;
  }
  .canvas {
    position: absolute;
    inset: 0;
    display: block;
    width: 100%;
    height: 100%;
  }

  .chip {
    position: absolute;
    top: 8px;
    transform: translateX(8px);
    pointer-events: none;
    min-width: 112px;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-well);
    background: var(--surface-2);
    box-shadow: var(--elev-raised);
    font-family: var(--font-mono);
    font-size: var(--type-xs);
  }
  .chip.flip {
    transform: translateX(-100%) translateX(-8px);
  }
  .chip-row {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    line-height: 1.5;
  }
  .chip-row span {
    color: var(--text-soft);
  }
  .chip-row b {
    color: var(--text);
    font-weight: 600;
  }
</style>
