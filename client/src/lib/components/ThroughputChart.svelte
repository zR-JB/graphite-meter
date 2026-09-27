<script lang="ts">
  import { onMount } from "svelte";
  import { inView } from "../actions/inView";
  import { handoff, nextFrame } from "../presentation/motion.svelte";
  import { store } from "../state/store.svelte";
  import {
    ChartEngine,
    type ChartData,
    type ChartPresentation,
    type HoverInfo,
  } from "../canvas/ChartEngine";
  import {
    fmtDuration,
    fmtSpeed,
    formatLatency,
    formatRate,
    rateUnit,
    rateValueAt,
  } from "../format";
  import { MISSING, phaseLabel, STAGE } from "../presentation/vocabulary";
  import { latencyOverflowGlyph } from "../canvas/latencyGlyph";
  import { fmtGaugeTick } from "./gaugeScale";
  import { watchCanvasPixelRatio } from "../canvas/canvasResolution";

  let canvasEl = $state<HTMLCanvasElement>();
  let plotEl = $state<HTMLDivElement>();
  let hover = $state.raw<HoverInfo | null>(null);
  let chartPresentation = $state.raw<ChartPresentation | null>(null);
  // Axis labels name the ceiling the plot settles on and change only while faded.
  const axis = handoff(
    () => {
      if (!chartPresentation) return { unit: "", left: [], right: [] };
      const { layout, units, ceiling } = chartPresentation;
      const { base, kind, index } = units;
      const label = (value: number) => (row: (typeof layout.axisRows)[0]) => ({
        y: row.y,
        text: fmtGaugeTick(value * (1 - row.fraction)),
      });
      return {
        unit: chartPresentation.hasThroughputScale
          ? rateUnit(base, kind, index)
          : "",
        left: chartPresentation.hasThroughputScale
          ? layout.axisRows.map(
              label(rateValueAt(ceiling.bytesPerSec, base, kind, index)),
            )
          : [],
        right: chartPresentation.latencyEnabled
          ? layout.axisRows.map(label(ceiling.rttMs))
          : [],
      };
    },
    ({ unit, left, right }) =>
      [unit, ...[...left, ...right].map((row) => row.text)].join(),
  );
  let selectedT = $state<number | null>(null);
  let retainSelection = false;
  const componentId = $props.id();
  const instructionsId = `${componentId}-instructions`;
  const viewport = $derived(chartPresentation?.layout.viewport);
  const selectedTime = $derived(selectedT ?? viewport?.tMin ?? 0);
  const hasData = $derived(
    store.throughput.length > 0 || store.latency.length > 0,
  );
  const rows = $derived.by(() => {
    if (!hover) return [];
    const units = { base: store.unitBase, kind: store.unitKind };
    const rate = (bytesPerSec: number) =>
      formatRate(bytesPerSec, units, store.scales.unitIndex);
    const rows: { label: string; value: string }[] = [];
    if (hover.bytesPerSec != null)
      rows.push({ label: "Throughput", value: rate(hover.bytesPerSec) });
    if (hover.downBytesPerSec != null)
      rows.push({
        label: STAGE.download.label,
        value: rate(hover.downBytesPerSec),
      });
    if (hover.upBytesPerSec != null)
      rows.push({
        label: STAGE.upload.label,
        value: rate(hover.upBytesPerSec),
      });
    if (chartPresentation?.latencyEnabled)
      rows.push({
        label: "Latency median",
        value: formatLatency(hover.rtt),
      });
    if (hover.timeoutCount > 0)
      rows.push({
        label: "Probe timeouts",
        value: `${hover.timeoutCount} of ${hover.pingCount}`,
      });
    return rows;
  });
  const selectionText = $derived.by(() => {
    if (!hasData) return "No measurements yet";
    const at = fmtDuration(hover?.t ?? selectedTime);
    return rows.some((row) => row.value !== MISSING)
      ? [at, ...rows.map((row) => `${row.label} ${row.value}`)].join(", ")
      : `${at}, no measurements at this position`;
  });
  let stopPointerFrame: (() => void) | null = null;
  let pointerClientX = 0;
  let pointerClientY = 0;
  let pointerY = $state<number | null>(null);
  let chipWidth = $state(220);
  let chipHeight = $state(100);
  const inspectorDots = $derived.by(() => {
    if (!hover || !chartPresentation) return [];
    const { layout } = chartPresentation;
    const dots: { key: string; x: number; y: number; color: string }[] = [];
    for (const [key, value, color] of [
      ["rate", hover.bytesPerSec, "var(--brand)"],
      ["down", hover.downBytesPerSec, "var(--phase-download)"],
      ["up", hover.upBytesPerSec, "var(--phase-upload)"],
    ] as const)
      if (value != null)
        dots.push({ key, x: hover.x, y: layout.throughputY(value), color });
    if (hover.rtt != null && hover.latencyX != null)
      dots.push({
        key: "latency",
        x: hover.latencyX,
        y:
          hover.latencyOverflow && hover.rtt >= layout.viewport.rttMax
            ? latencyOverflowGlyph(layout.plot.top).dot.y
            : layout.latencyY(hover.rtt),
        color: hover.rttLoaded
          ? "var(--latency-loaded)"
          : "var(--phase-latency)",
      });
    return dots;
  });
  const chipPosition = $derived.by(() => {
    if (!hover || !chartPresentation) return { x: 8, y: 8 };
    const { layout } = chartPresentation;
    const anchor =
      pointerY == null
        ? inspectorDots[0]
        : inspectorDots.reduce<(typeof inspectorDots)[number] | undefined>(
            (nearest, dot) =>
              !nearest ||
              Math.abs(dot.y - pointerY!) < Math.abs(nearest.y - pointerY!)
                ? dot
                : nearest,
            undefined,
          );
    const x = anchor?.x ?? hover.x;
    const y = anchor?.y ?? pointerY ?? layout.plot.top;
    return {
      x: Math.max(
        8,
        Math.min(
          x + 12 + chipWidth <= layout.width - 8 ? x + 12 : x - chipWidth - 12,
          layout.width - chipWidth - 8,
        ),
      ),
      y: Math.max(
        8,
        Math.min(y - chipHeight / 2, layout.height - chipHeight - 8),
      ),
    };
  });
  const timelineAt = (now: number) =>
    store.phaseStartedAtMs + store.phaseClock.at(now);
  const chartData = (): ChartData => ({
    throughput: store.throughput,
    throughputRevision: store.throughputRevision,
    latency: store.latency,
    latencyRevision: store.latencyRevision,
    latencyEnabled: store.latencyEnabled,
    phase: store.phase,
    phaseStartedAtMs: store.phaseStartedAtMs,
    timelineAt,
    runSeq: store.runSeq,
    scaleBytesPerSec: store.scales.chartBytesPerSec,
    latencyScaleMs: store.latencyScaleMs,
    units: {
      base: store.unitBase,
      kind: store.unitKind,
      index: store.scales.unitIndex,
    },
    resultRates: {
      download: store.stageResults.download?.reportedBytesPerSec,
      upload: store.stageResults.upload?.reportedBytesPerSec,
      bidiDown: store.result?.bidirectional?.down?.reportedBytesPerSec,
      bidiUp: store.result?.bidirectional?.up?.reportedBytesPerSec,
    },
  });
  const engine = new ChartEngine(
    chartData(),
    (next) => {
      chartPresentation = next;
      setTimeScale(next.layout.viewport.tMax);
      updateHover();
    },
    (tMax) => {
      setTimeScale(tMax);
      if (selectedT != null) updateHover();
    },
    (opacity) => plotEl?.style.setProperty("--run-fade", String(opacity)),
  );
  $effect(() => engine.update(chartData()));

  function onMove(e: PointerEvent) {
    if (e.pointerType !== "mouse") return;
    pointerClientX = e.clientX;
    pointerClientY = e.clientY;
    // Coalesce pointer events into one small DOM update. The cached chart stays parked.
    stopPointerFrame ??= nextFrame(() => {
      stopPointerFrame = null;
      if (!document.hidden && plotEl) {
        pointerY = pointerClientY - plotEl.getBoundingClientRect().top;
        selectX(pointerClientX - plotEl.getBoundingClientRect().left);
      }
    });
  }
  function selectX(x: number) {
    if (!chartPresentation || !hasData) return;
    hover = engine.inspect(x);
    selectedT = hover?.t ?? null;
  }
  function selectTime(t: number) {
    const { tMin, tMax } = engine.viewport;
    selectedT = Math.max(tMin, Math.min(tMax, t));
    updateHover();
  }
  function onKeyDown(e: KeyboardEvent) {
    if (!chartPresentation || !hasData) return;
    pointerY = null;
    const current = engine.viewport;
    const step = (current.tMax - current.tMin) / 100;
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
        selectTime(current.tMin);
        break;
      case "End":
        selectTime(current.tMax);
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
    pointerY = null;
    retainSelection = true;
    selectTime(selectedTime);
  }
  function onPointerUp(e: PointerEvent) {
    if (e.pointerType === "mouse") return;
    plotEl?.focus({ preventScroll: true });
    retainSelection = true;
    pointerY =
      e.clientY -
      (e.currentTarget as HTMLDivElement).getBoundingClientRect().top;
    selectX(
      e.clientX -
        (e.currentTarget as HTMLDivElement).getBoundingClientRect().left,
    );
  }
  function updateHover() {
    hover = selectedT == null ? null : engine.inspectTime(selectedT);
  }
  function clearSelection() {
    stopPointerFrame?.();
    stopPointerFrame = null;
    selectedT = null;
    hover = null;
  }
  function setTimeScale(tMax: number) {
    plotEl?.style.setProperty("--t-max", String(tMax));
  }
  function onLeave() {
    if (!retainSelection) clearSelection();
  }
  function onBlur() {
    retainSelection = false;
    clearSelection();
  }

  onMount(() => {
    engine.attach(canvasEl!);

    const themeObserver = new MutationObserver(() => engine.invalidateTheme());
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    const resizeObserver = new ResizeObserver(() => engine.resize());
    resizeObserver.observe(canvasEl!);
    const stopWatchingPixelRatio = watchCanvasPixelRatio(() =>
      engine.invalidateTheme(),
    );

    return () => {
      engine.destroy();
      stopPointerFrame?.();
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
    <canvas
      bind:this={canvasEl}
      class="canvas"
      aria-hidden="true"
      {@attach inView((seen) => (engine.visible = seen))}
    ></canvas>

    {#if chartPresentation}
      {@const presentation = chartPresentation}
      {@const { plot, width } = presentation.layout}
      {@const { base, kind, index } = presentation.units}
      {@const rate = (bytesPerSec: number) =>
        rateValueAt(bytesPerSec, base, kind, index)}
      <div class="chart-labels" aria-hidden="true">
        <div style:opacity={axis.opacity}>
          {#if axis.shown.unit}
            <span class="axis-unit" style:left="4px" style:top={`${plot.top}px`}
              >{axis.shown.unit}</span
            >
          {/if}
          {#each axis.shown.left as row (row.y)}
            <span class="axis-label" style:left="4px" style:top={`${row.y}px`}
              >{row.text}</span
            >
          {/each}
          {#if axis.shown.right.length}
            <span
              class="axis-unit axis-right"
              style:left={`${width - 4}px`}
              style:top={`${plot.top}px`}>ms</span
            >
          {/if}
          {#each axis.shown.right as row (row.y)}
            <span
              class="axis-label axis-right"
              style:left={`${width - 4}px`}
              style:top={`${row.y}px`}>{row.text}</span
            >
          {/each}
        </div>
        {#each presentation.layout.timeMajorTicks as tick (tick.t)}
          {@const { left, right } = presentation.layout.plot}
          <span
            class="time-label"
            style:translate={`calc(${left}px + ${right - left}px * ${tick.t} / var(--t-max) - 50%) -100%`}
            style:top={`${presentation.layout.timeLabelY}px`}
            >{fmtDuration(tick.t, tick.t % 1000 === 0 ? 0 : 1)}</span
          >
        {/each}
        <div class="marks">
          {#each presentation.phaseLabels as label (label.phase)}
            <span
              class="phase-label caps"
              style:left={`${label.x}px`}
              style:top={`${label.y}px`}
              >{label.phase === "bidirectional"
                ? STAGE.bidirectional.short
                : phaseLabel(label.phase)}</span
            >
          {/each}
          {#each presentation.phaseStats as stat (stat.lane)}
            <span
              class="stat-label"
              data-tone={stat.tone}
              style:left={`${stat.x}px`}
              style:top={`${stat.y}px`}
              >{fmtSpeed(rate(stat.bytesPerSec))}
              {rateUnit(base, kind, index)}</span
            >
          {/each}
        </div>
      </div>
    {/if}

    {#if hover && chartPresentation}
      <div
        class="inspection-guide"
        style:top={`${chartPresentation.layout.plot.top}px`}
        style:height={`${chartPresentation.layout.plot.bottom - chartPresentation.layout.plot.top}px`}
        style:transform={`translateX(${hover.x}px)`}
        aria-hidden="true"
      ></div>
      {#each inspectorDots as dot (dot.key)}
        <span
          class="inspection-dot"
          style:background={dot.color}
          style:transform={`translate(${dot.x}px, ${dot.y}px)`}
          aria-hidden="true"
        ></span>
      {/each}
      <div
        class="inspect-card chip"
        bind:offsetWidth={chipWidth}
        bind:offsetHeight={chipHeight}
        style:transform={`translate(${chipPosition.x}px, ${chipPosition.y}px)`}
      >
        <div class="chip-row">
          <span>Time</span><b>{fmtDuration(hover.t)}</b>
        </div>
        {#each rows as row (row.label)}
          <div class="chip-row"><span>{row.label}</span><b>{row.value}</b></div>
        {/each}
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
  }
  /* Secondary to the gauge hero: a shallow recess filling the granted height. */
  .plot {
    position: relative;
    flex: 1 1 auto;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
  }
  .canvas,
  .chart-labels,
  .marks {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  .chart-labels,
  .inspection-guide,
  .inspection-dot {
    pointer-events: none;
  }
  .inspection-guide,
  .inspection-dot {
    position: absolute;
    left: 0;
    will-change: transform;
  }
  .inspection-guide {
    width: 1px;
    background: var(--brand);
  }
  .inspection-dot {
    top: 0;
    width: 5px;
    height: 5px;
    margin: -2.5px;
    border-radius: var(--r-full);
  }
  .chart-labels {
    color: var(--text-soft);
    font: var(--type-2xs) var(--font-mono);
  }
  .axis-label,
  .axis-unit,
  .time-label,
  .phase-label,
  .stat-label {
    position: absolute;
    white-space: nowrap;
  }
  .axis-label {
    --x: 0;
    translate: var(--x) -50%;
  }
  .axis-unit {
    --x: 0;
    translate: var(--x) calc(-100% - 7px);
    color: var(--text-muted);
  }
  .axis-right {
    --x: -100%;
  }
  /* Moving labels translate rather than lay out again as the time scale eases. */
  .time-label {
    left: 0;
  }
  /* The run's own labels leave with its plot; the axes hand off by themselves. */
  .time-label,
  .marks {
    opacity: var(--run-fade, 1);
  }
  .phase-label {
    translate: 0 -100%;
    opacity: 0.62;
  }
  .phase-label,
  .stat-label {
    transition: opacity var(--dur-slide) var(--ease-out);
  }
  @starting-style {
    .phase-label,
    .stat-label {
      opacity: 0;
    }
  }
  .stat-label {
    max-width: 126px;
    overflow: hidden;
    padding: 2px 5px;
    border: 1px solid var(--tone);
    border-radius: var(--r-well);
    background: var(--surface-1);
    color: var(--tone);
    font-weight: var(--w-heavy);
    text-overflow: ellipsis;
  }
  .chip {
    top: 0;
    left: 0;
    width: 224px;
    min-width: 112px;
    max-width: calc(100% - 2 * var(--space-2));
    font: var(--type-xs) / 1.5 var(--font-mono);
    will-change: transform;
  }
  .chip-row {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .chip-row span {
    color: var(--text-soft);
  }
  .chip-row b {
    font-weight: var(--w-strong);
  }
</style>
