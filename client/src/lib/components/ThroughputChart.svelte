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
  let position = $state<number | null>(null);
  let retainSelection = false;
  const componentId = $props.id();
  const instructionsId = `${componentId}-instructions`;
  const viewport = $derived(chartPresentation?.layout.viewport);
  const selectedTime = $derived(
    (viewport?.tMin ?? 0) +
      (position ?? 0) * ((viewport?.tMax ?? 0) - (viewport?.tMin ?? 0)),
  );
  const hasData = $derived(
    store.throughput.length > 0 || store.latency.length > 0,
  );
  const selectionText = $derived.by(() => {
    if (!hasData) return "No measurements yet";
    const details = [
      `${((hover?.t ?? selectedTime) / 1000).toFixed(1)} seconds`,
    ];
    if (!hover) return `${details[0]}, no measurements at this position`;
    if (hover.bytesPerSec != null)
      details.push(
        `rate ${fmtSpeed(store.toUnit(hover.bytesPerSec))} ${store.unitLabel}`,
      );
    if (hover.downBytesPerSec != null)
      details.push(
        `download ${fmtSpeed(store.toUnit(hover.downBytesPerSec))} ${store.unitLabel}`,
      );
    if (hover.upBytesPerSec != null)
      details.push(
        `upload ${fmtSpeed(store.toUnit(hover.upBytesPerSec))} ${store.unitLabel}`,
      );
    if (hover.rtt != null)
      details.push(`bucket median latency ${fmtMs(hover.rtt)} milliseconds`);
    if (hover.pingCount > 0)
      details.push(
        `probe timeouts ${hover.lossCount} of ${hover.pingCount} resolved probes in bucket`,
      );
    return details.join(", ");
  });
  let hoverPresentation: PresentationHandle;
  // presentation keeps animating while a render returns true.
  const PARKED = false;

  // Invalidate only for state read by ChartEngine.
  $effect(() => {
    void store.phase;
    void store.runSeq;
    void store.throughput.length;
    void store.throughputRevision;
    void store.latency.length;
    void store.latencyRevision;
    void store.phaseElapsedMs;
    void store.latencyEnabled;
    void store.chartScaleBytesPerSec; // re-arm if the chart scale / pinned ceiling shifts while parked
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

  function onMove(e: PointerEvent) {
    if (e.pointerType !== "mouse") return;
    selectX(
      e.clientX -
        (e.currentTarget as HTMLDivElement).getBoundingClientRect().left,
    );
  }
  function selectX(x: number) {
    if (!chartPresentation || !hasData) return;
    const { plot } = chartPresentation.layout;
    position =
      (Math.max(plot.left, Math.min(plot.right, x)) - plot.left) /
      (plot.right - plot.left);
    hoverPresentation?.invalidate();
  }
  function selectTime(t: number) {
    if (chartPresentation) selectX(chartPresentation.layout.x(t));
  }
  function onKeyDown(e: KeyboardEvent) {
    if (!viewport || !hasData) return;
    const step = (viewport.tMax - viewport.tMin) / 100;
    switch (e.key) {
      case "ArrowRight":
      case "ArrowUp":
        selectTime(selectedTime + step);
        break;
      case "ArrowLeft":
      case "ArrowDown":
        selectTime(selectedTime - step);
        break;
      case "Home":
        selectTime(viewport.tMin);
        break;
      case "End":
        selectTime(viewport.tMax);
        break;
      case "Escape":
        clearSelection();
        break;
      default:
        return;
    }
    e.preventDefault();
    e.stopPropagation();
  }
  function onFocus() {
    retainSelection = true;
    selectTime(position == null ? (viewport?.tMin ?? 0) : selectedTime);
  }
  function onPointerUp(e: PointerEvent) {
    if (e.pointerType === "mouse") return;
    plotEl?.focus({ preventScroll: true });
    retainSelection = true;
    selectX(
      e.clientX -
        (e.currentTarget as HTMLDivElement).getBoundingClientRect().left,
    );
  }
  // A one-shot repaint, re-armed by invalidate().
  function updateHover() {
    engine.setHover(
      position == null ? null : chartPresentation!.layout.x(selectedTime),
    );
    hover = engine.hoverInfo();
    return PARKED;
  }
  function clearSelection() {
    position = null;
    hoverPresentation?.invalidate();
  }
  function onLeave() {
    if (!retainSelection) clearSelection();
  }
  function onBlur() {
    retainSelection = false;
    clearSelection();
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
        scaleBytesPerSec: store.chartScaleBytesPerSec,
        latencyScaleMs: store.latencyScaleMs,
        resultRates: {
          download: store.stageResults.download?.reportedBytesPerSec,
          upload: store.stageResults.upload?.reportedBytesPerSec,
          bidiDown: store.result?.bidirectional?.down?.reportedBytesPerSec,
          bidiUp: store.result?.bidirectional?.up?.reportedBytesPerSec,
        },
      }),
      (next) => {
        chartPresentation = next;
        const { plot } = next.layout;
        engine.setHover(
          position == null
            ? null
            : plot.left + position * (plot.right - plot.left),
        );
        hover = engine.hoverInfo();
      },
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
    role="slider"
    tabindex={hasData ? 0 : -1}
    aria-label="Throughput and latency over time"
    aria-describedby={instructionsId}
    aria-disabled={!hasData}
    aria-valuemin={viewport?.tMin ?? 0}
    aria-valuemax={viewport?.tMax ?? 0}
    aria-valuenow={Math.max(
      viewport?.tMin ?? 0,
      Math.min(viewport?.tMax ?? 0, selectedTime),
    )}
    aria-valuetext={selectionText}
    onpointermove={onMove}
    onpointerleave={onLeave}
    onpointerup={onPointerUp}
    onkeydown={onKeyDown}
    onfocus={onFocus}
    onblur={onBlur}
  >
    <canvas bind:this={canvasEl} class="canvas" aria-hidden="true"></canvas>

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
        {#each presentation.phaseStats as stat (stat.lane)}
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
        style:left={`clamp(8px, ${hover.x + 8}px, calc(100% - 232px))`}
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
        {#if hover.pingCount > 0}
          <div class="chip-row">
            <span>probe timeouts</span><b>{hover.lossCount}/{hover.pingCount}</b
            >
          </div>
        {/if}
      </div>
    {/if}
  </div>
  <span id={instructionsId} class="sr-only"
    >Use arrow keys to inspect the timeline, Home and End to jump to its edges,
    and Escape to dismiss details. On touch screens, tap the chart. Rates are
    plotted values; latency and probe timeouts describe the nearby display
    bucket, not whole-run statistics.</span
  >
</section>

<style>
  /* One chart surface preserves plot space without a second padded frame. */
  .chart {
    display: flex;
    flex-direction: column;
    min-height: 142px;
  }
  /* Secondary to the gauge hero: a shallow recess in the tile, filling the
     granted height down to the compact floor. */
  .plot {
    position: relative;
    flex: 1 1 auto;
    min-height: 140px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
    overflow: hidden;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .plot:focus-visible {
    outline: 2px solid var(--text);
    outline-offset: 2px;
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
    width: 224px;
    max-width: calc(100% - 16px);
    box-sizing: border-box;
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
