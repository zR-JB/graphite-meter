<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { ChartEngine, type HoverInfo } from "../canvas/ChartEngine";
  import { fmtSpeed, fmtMs } from "../format";
  import {
    presentation,
    type PresentationHandle,
  } from "../canvas/presentation";

  let canvasEl = $state<HTMLCanvasElement>();
  let plotEl = $state<HTMLDivElement>();
  let engine: ChartEngine;
  let hover = $state<HoverInfo | null>(null);
  let hoverX: number | null = null;
  let hoverPresentation: PresentationHandle;

  // Invalidate only for state read by ChartEngine.
  $effect(() => {
    void store.phase;
    void store.runSeq;
    void store.throughput.length;
    void store.latency.length;
    void store.latencyEnabled;
    void store.displayScaleBytesPerSec; // re-arm if the shared scale / pinned ceiling shifts while parked
    void store.stageResults.download;
    void store.stageResults.upload;
    void store.result?.bidirectional;
    // Unit/base toggles only change the axis tick FORMAT, not the data — but the
    // loop parks once a finished run settles, so without tracking them here a
    // post-run toggle would never re-wake the loop and the ticks would freeze.
    void store.unitBase;
    void store.unitKind;
    // unitLabel changes whenever the shared prefix index (k/M/G/T) moves —
    // that can happen from the raw peak alone, independent of
    // displayScaleBytesPerSec (the DWELL-FILTERED sustained peak, a
    // different signal) — so track it directly too.
    void store.unitLabel;
    engine?.wake();
  });

  function onMove(e: MouseEvent) {
    hoverX = e.offsetX;
    hoverPresentation?.invalidate();
  }
  function updateHover() {
    engine.setHover(hoverX);
    hover = engine.hoverInfo();
    if (
      hover &&
      hover.bytesPerSec == null &&
      hover.downBytesPerSec == null &&
      hover.upBytesPerSec == null &&
      hover.rtt == null
    )
      hover = null;
    return false;
  }
  function onLeave() {
    hoverX = null;
    hoverPresentation?.invalidate();
  }

  onMount(() => {
    engine = new ChartEngine(
      () => ({
        throughput: store.throughput,
        latency: store.latency, // raw — the engine buckets the LINE itself; axis/hover use raw
        latencyEnabled: store.latencyEnabled,
        phase: store.phase,
        phaseStartedAtMs: store.phaseStartedAtMs,
        runSeq: store.runSeq,
        scaleBytesPerSec: store.displayScaleBytesPerSec,
        resultRates: {
          download: store.stageResults.download?.reportedBytesPerSec,
          upload: store.stageResults.upload?.reportedBytesPerSec,
          bidiDown: store.result?.bidirectional?.down.reportedBytesPerSec,
          bidiUp: store.result?.bidirectional?.up.reportedBytesPerSec,
        },
      }),
      {
        throughput: (bytesPerSec) => fmtSpeed(store.toUnit(bytesPerSec)),
        latency: (rtt) => fmtMs(rtt),
      },
    );
    engine.attach(canvasEl!);
    hoverPresentation = presentation.register(plotEl!, updateHover);

    const mo = new MutationObserver(() => engine.invalidateTheme());
    mo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    const ro = new ResizeObserver(() => engine.invalidateTheme());
    ro.observe(canvasEl!);

    return () => {
      engine.destroy();
      hoverPresentation.destroy();
      mo.disconnect();
      ro.disconnect();
    };
  });
</script>

<section class="chart">
  <div
    bind:this={plotEl}
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
        <div class="chip-row">
          <span>t</span><b>{(hover.t / 1000).toFixed(1)}s</b>
        </div>
        {#if hover.bytesPerSec != null}
          <div class="chip-row">
            <span>rate</span><b
              >{fmtSpeed(store.toUnit(hover.bytesPerSec))} {store.unitLabel}</b
            >
          </div>
        {/if}
        {#if hover.downBytesPerSec != null}
          <div class="chip-row">
            <span>down</span><b
              >{fmtSpeed(store.toUnit(hover.downBytesPerSec))}
              {store.unitLabel}</b
            >
          </div>
        {/if}
        {#if hover.upBytesPerSec != null}
          <div class="chip-row">
            <span>up</span><b
              >{fmtSpeed(store.toUnit(hover.upBytesPerSec))}
              {store.unitLabel}</b
            >
          </div>
        {/if}
        {#if hover.rtt != null}
          <div class="chip-row">
            <span>rtt</span><b>{fmtMs(hover.rtt)} ms</b>
          </div>
        {/if}
      </div>
    {/if}
  </div>
</section>

<style>
  /* Flat milled tile on the faceplate (quieter than the gauge well). A flex
     column so the plot can stretch when the console stage grants this section
     extra height (desktop; see Console's .stage > .chart rule) while the
     140px floor keeps it legible when it doesn't (mobile/stacked flow). */
  .chart {
    display: flex;
    flex-direction: column;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  /* Secondary to the gauge hero: fills whatever height the tile is granted,
     never less than the compact floor. The plot screen is a shallow recess
     set into the tile. */
  .plot {
    position: relative;
    flex: 1 1 auto;
    min-height: 140px;
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
    gap: var(--space-3);
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
