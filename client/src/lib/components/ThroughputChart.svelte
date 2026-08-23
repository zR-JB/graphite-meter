<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import {
    ChartEngine,
    type ChartLabelPhase,
    type ChartPresentation,
    type HoverInfo,
  } from "../canvas/ChartEngine";
  import { fmtSpeed, fmtMs } from "../format";
  import {
    presentation,
    type PresentationHandle,
  } from "../canvas/presentation";
  import { watchCanvasPixelRatio } from "../canvas/canvasResolution";

  const PHASE_LABEL: Record<ChartLabelPhase, string> = {
    warmup: "WARM-UP",
    latency: "PING",
    download: "DOWNLOAD",
    upload: "UPLOAD",
    bidirectional: "BI-DIR",
  } as const;

  let canvasEl = $state<HTMLCanvasElement>();
  let plotEl = $state<HTMLDivElement>();
  let engine: ChartEngine;
  let hover = $state<HoverInfo | null>(null);
  let chartPresentation = $state<ChartPresentation | null>(null);
  let hoverX: number | null = null;
  let hoverPresentation: PresentationHandle;
  // presentation keeps animating while a render returns true.
  const PARKED = false;

  // Invalidate only for state read by ChartEngine.
  $effect(() => {
    void store.phase;
    void store.runSeq;
    void store.throughput.length;
    void store.latency.length;
    void store.latencyRevision;
    void store.phaseElapsedMs;
    void store.latencyEnabled;
    void store.displayScaleBytesPerSec; // re-arm if the shared scale / pinned ceiling shifts while parked
    void store.latencyScaleMs;
    void store.stageResults.download;
    void store.stageResults.upload;
    void store.result?.bidirectional;
    // Tick format only, but the loop parks after a run: tracking these re-arms
    // it. unitLabel also moves with the raw peak's k/M/G/T prefix on its own.
    void store.unitBase;
    void store.unitKind;
    void store.unitLabel;
    engine?.wake();
  });

  function onMove(e: MouseEvent) {
    hoverX = e.offsetX;
    hoverPresentation?.invalidate();
  }
  // A one-shot repaint, re-armed by invalidate().
  function updateHover() {
    engine.setHover(hoverX);
    hover = engine.hoverInfo();
    return PARKED;
  }
  function onLeave() {
    hoverX = null;
    hoverPresentation?.invalidate();
  }

  onMount(() => {
    engine = new ChartEngine(
      () => ({
        throughput: store.throughput,
        latency: store.latency, // raw event-time buckets drive glyphs, axes, and hover
        latencyRevision: store.latencyRevision,
        latencyEnabled: store.latencyEnabled,
        phase: store.phase,
        phaseStartedAtMs: store.phaseStartedAtMs,
        timelineT: Math.max(
          store.phaseStartedAtMs + store.phaseElapsedMs,
          store.throughput.at(-1)?.t ?? 0,
          store.latency.at(-1)?.endT ?? 0,
        ),
        runSeq: store.runSeq,
        scaleBytesPerSec: store.displayScaleBytesPerSec,
        latencyScaleMs: store.latencyScaleMs,
        resultRates: {
          download: store.stageResults.download?.reportedBytesPerSec,
          upload: store.stageResults.upload?.reportedBytesPerSec,
          bidiDown: store.result?.bidirectional?.down?.reportedBytesPerSec,
          bidiUp: store.result?.bidirectional?.up?.reportedBytesPerSec,
        },
      }),
      (next) => (chartPresentation = next),
    );
    engine.attach(canvasEl!);
    hoverPresentation = presentation.register(plotEl!, updateHover);

    const themeObserver = new MutationObserver(() => engine.invalidateTheme());
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    const resizeObserver = new ResizeObserver(() => engine.invalidateTheme());
    resizeObserver.observe(canvasEl!);
    const stopWatchingPixelRatio = watchCanvasPixelRatio(() =>
      engine.invalidateTheme(),
    );

    return () => {
      engine.destroy();
      hoverPresentation.destroy();
      themeObserver.disconnect();
      resizeObserver.disconnect();
      stopWatchingPixelRatio();
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

    {#if chartPresentation}
      {@const presentation = chartPresentation}
      <div class="chart-labels" aria-hidden="true">
        {#if presentation.hasThroughputScale}
          {#each presentation.layout.axisRows as row (row.fraction)}
            <span
              class="axis-label axis-label-left"
              style:left="4px"
              style:top={`${row.y}px`}
              >{fmtSpeed(
                store.toUnit(
                  presentation.layout.viewport.bytesPerSecMax *
                    (1 - row.fraction),
                ),
              )}</span
            >
          {/each}
        {/if}
        {#if presentation.latencyEnabled}
          {#each presentation.layout.axisRows as row (row.fraction)}
            <span
              class="axis-label axis-label-right"
              style:left={`${presentation.layout.width - 4}px`}
              style:top={`${row.y}px`}
              >{fmtMs(
                presentation.layout.viewport.rttMin +
                  (presentation.layout.viewport.rttMax -
                    presentation.layout.viewport.rttMin) *
                    (1 - row.fraction),
              )}</span
            >
          {/each}
        {/if}
        {#each presentation.layout.timeMajorTicks as tick (tick.t)}
          <span
            class="time-label"
            style:left={`${tick.x}px`}
            style:top={`${presentation.layout.timeLabelY}px`}
            >{tick.t % 1000 === 0
              ? `${tick.t / 1000}s`
              : `${(tick.t / 1000).toFixed(1)}s`}</span
          >
        {/each}
        {#each presentation.phaseLabels as label (label.phase + label.x)}
          <span
            class="phase-label"
            style:left={`${label.x}px`}
            style:top={`${label.y}px`}>{PHASE_LABEL[label.phase]}</span
          >
        {/each}
        {#each presentation.phaseStats as stat (stat.x + stat.y + stat.bytesPerSec)}
          <span
            class="stat-label"
            style:border-color={stat.stroke}
            style:color={stat.stroke}
            style:left={`${stat.x}px`}
            style:top={`${stat.y}px`}
            >{fmtSpeed(store.toUnit(stat.bytesPerSec))} {store.unitLabel}</span
          >
        {/each}
      </div>
    {/if}

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
            <span>median</span><b>{fmtMs(hover.rtt)} ms</b>
          </div>
        {/if}
        {#if hover.lossCount > 0}
          <div class="chip-row">
            <span>loss</span><b>{hover.lossCount}/{hover.pingCount}</b>
          </div>
        {/if}
      </div>
    {/if}
  </div>
</section>

<style>
  /* Flat milled tile on the faceplate, quieter than the gauge well. The flex
     column lets the plot stretch into whatever height Console's
     .stage > .chart rule grants, down to the 140px floor in stacked flow. */
  .chart {
    display: flex;
    flex-direction: column;
    min-height: 164px;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  /* Secondary to the gauge hero: a shallow recess in the tile, filling the
     granted height down to the compact floor. */
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
  .chart-labels {
    position: absolute;
    inset: 0;
    pointer-events: none;
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-soft);
  }
  .axis-label,
  .time-label,
  .phase-label,
  .stat-label {
    position: absolute;
    white-space: nowrap;
  }
  .axis-label {
    transform: translateY(-50%);
  }
  .axis-label-right {
    transform: translate(-100%, -50%);
  }
  .time-label {
    transform: translate(-50%, -100%);
  }
  .phase-label {
    transform: translateY(-100%);
    font-size: 9px;
    font-weight: 700;
    opacity: 0.62;
  }
  .stat-label {
    max-width: 126px;
    overflow: hidden;
    padding: 2px 5px;
    border: 1px solid color-mix(in srgb, var(--text-soft) 55%, transparent);
    border-radius: 4px;
    background: var(--surface-1);
    color: var(--text-soft);
    font-size: 9px;
    font-weight: 700;
    text-overflow: ellipsis;
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
