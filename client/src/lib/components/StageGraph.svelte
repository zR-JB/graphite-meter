<script lang="ts">
  import { untrack } from "svelte";
  import { warmUp } from "../actions/intent";
  import { scrub } from "../actions/scrub";
  import { inView } from "../actions/inView";
  import { fmtDuration, fmtMs, formatLatency } from "../format";
  import {
    nearestAt,
    stageGraphGeometry,
    drawStageGraph,
    type GraphPoint,
    type LatencyPoint,
  } from "../presentation/stageGraph";

  interface Props {
    tone: "download" | "upload" | "bidirectional" | "latency";
    lanes: GraphPoint[][];
    /** Names the lanes in the readout when there are two. */
    laneNames?: string[];
    latency: LatencyPoint[];
    start: number;
    span: number;
    ceiling: number;
    baseline: number | null;
    latencyTop: number;
    head?: { t: number; values: (number | null)[] } | null;
    rate: (bytesPerSec: number) => string;
    label: string;
  }
  let {
    tone,
    lanes,
    laneNames = [],
    latency,
    start,
    span,
    ceiling,
    baseline,
    latencyTop,
    head = null,
    rate,
    label,
  }: Props = $props();

  const uid = $props.id();
  const TRACK = 20;
  let width = $state(0);
  let plotHeight = $state(0);
  let trackHeight = $state(0);
  let seen = $state(false);
  // Without rate lanes the strip is the latency track alone, at the strip's full height.
  const trackOnly = $derived(lanes.length === 0);
  const trackH = $derived(trackOnly ? trackHeight : TRACK);
  const geometry = $derived(
    width && (trackOnly ? trackHeight : plotHeight)
      ? stageGraphGeometry({
          lanes,
          latency,
          start,
          span,
          ceiling,
          baseline,
          latencyTop,
          width,
          plotHeight: trackOnly ? 0 : plotHeight,
          trackHeight: trackH,
        })
      : null,
  );
  const graph = $derived(
    geometry && drawStageGraph(geometry, seen ? head : null),
  );
  const hasData = $derived(
    trackOnly
      ? latency.length > 0
      : !!geometry?.bins.some((lane) => lane.length),
  );

  let hoverT = $state<number | null>(null);
  let keyboard = false;
  // The readout reads measured samples; bins only draw, and their width changes with the card's.
  const samples = $derived(
    lanes.map((lane) => lane.filter((point) => point.t >= start)),
  );
  // A bar reads as what it draws: its replies' range, or the one value they share.
  type Row = { label: string; value: string; mark?: string };
  const replyRow = (bar: LatencyPoint): Row => ({
    label: "Latency",
    value:
      bar.lo < bar.hi
        ? `${fmtMs(bar.lo)}–${fmtMs(bar.hi)} ms`
        : formatLatency(bar.lo),
  });
  const hover = $derived.by(() => {
    if (hoverT === null || !geometry || !hasData) return null;
    if (trackOnly) {
      const bar = nearestAt(geometry.dots, hoverT!);
      if (!bar) return null;
      return {
        x: bar.x,
        rows: [replyRow(bar)],
        time: fmtDuration(Math.max(0, bar.t - start), 1),
      };
    }
    const rates = samples.map((lane) => nearestAt(lane, hoverT!));
    const at = rates.find(Boolean)!.t;
    const near = geometry.dots.find((bar) => at >= bar.t0 && at <= bar.t1);
    const rows = rates.map((point, lane): Row => ({
      label: laneNames[lane] ?? "Rate",
      value: point ? rate(point.v) : "",
      mark: lanes.length > 1 ? ["↓", "↑"][lane] : undefined,
    }));
    if (near) rows.push(replyRow(near));
    return {
      x: Math.min(width, Math.max(0, ((at - start) / (span || 1)) * width)),
      rows,
      time: fmtDuration(Math.max(0, at - start), 1),
    };
  });

  let box: HTMLElement | undefined = $state();
  let readoutWidth = $state(0);
  function clear() {
    if (!keyboard) hoverT = null;
  }
  // A readout answers the pointer at once; only explainers wait for a pause.
  const pointer = scrub({
    read(event) {
      if (!box) return;
      const rect = box.getBoundingClientRect();
      const ratio = (event.clientX - rect.left) / rect.width;
      hoverT = start + Math.min(1, Math.max(0, ratio)) * span;
    },
    clear,
  });
  function onLeave(event: PointerEvent) {
    if (event.pointerType === "touch") return;
    if (hoverT !== null) warmUp();
    clear();
  }
  // A new run's graph starts without the last one's reading.
  $effect(() => {
    void start;
    untrack(clear);
  });
  // Arrow keys walk the drawn bins from the newest; the readout names the sample nearest where it stops.
  function stepTo(key: string): boolean {
    const times = trackOnly
      ? geometry?.dots.map((bar) => bar.t)
      : geometry?.bins.find((lane) => lane.length)?.map((p) => p.t);
    if (!times?.length) return false;
    const index =
      hoverT === null
        ? times.length
        : times.indexOf(
            nearestAt(
              times.map((t) => ({ t })),
              hoverT,
            )!.t,
          );
    const next = (
      {
        ArrowLeft: index - 1,
        ArrowRight: index + 1,
        Home: 0,
        End: times.length - 1,
      } as Record<string, number>
    )[key];
    if (next === undefined) return false;
    hoverT = times[Math.min(times.length - 1, Math.max(0, next))];
    return true;
  }
  function onKey(event: KeyboardEvent) {
    if (event.key === "Escape") {
      if (hoverT === null) return;
      hoverT = null;
    } else if (!stepTo(event.key)) return;
    event.preventDefault();
  }
</script>

<div
  {@attach inView((value) => (seen = value))}
  class="graph"
  data-tone={tone}
  bind:this={box}
  bind:clientWidth={width}
  role="slider"
  tabindex={hasData ? 0 : -1}
  aria-label={label}
  aria-valuemin={0}
  aria-valuemax={span}
  aria-valuenow={hoverT === null ? span : Math.round(hoverT - start)}
  aria-valuetext={hover
    ? `${hover.time}: ${hover.rows.map((row) => `${row.label} ${row.value}`).join(", ")}`
    : undefined}
  onpointerdown={pointer.down}
  onpointermove={pointer.move}
  onpointerup={pointer.up}
  onpointercancel={pointer.cancel}
  onpointerleave={onLeave}
  onfocus={(event) => {
    if (!(event.currentTarget as HTMLElement).matches(":focus-visible")) return;
    keyboard = true;
    stepTo("End");
  }}
  onblur={() => {
    keyboard = false;
    hoverT = null;
  }}
  onkeydown={onKey}
>
  {#if !trackOnly}<div class="plot" bind:clientHeight={plotHeight}>
      {#if graph}
        <svg {width} height={plotHeight} aria-hidden="true">
          <defs>
            <linearGradient id="{uid}-fill" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stop-color="var(--tone)" stop-opacity="0.2" />
              <stop offset="1" stop-color="var(--tone)" stop-opacity="0" />
            </linearGradient>
          </defs>
          <line
            class="axis"
            x1="0"
            x2={width}
            y1={plotHeight - 0.5}
            y2={plotHeight - 0.5}
          />
          {#if graph.area}<path
              class="area"
              d={graph.area}
              fill="url(#{uid}-fill)"
            />{/if}
          {#each graph.lines as line, index (index)}
            <path class="line" class:second={index > 0} d={line} />
          {/each}
          {#each graph.heads as dot, index (index)}
            <circle class="head" cx={dot.x} cy={dot.y} r="3.5" />
          {/each}
          {#if hover}<line
              class="cursor"
              x1={hover.x}
              x2={hover.x}
              y1="0"
              y2={plotHeight}
            />{/if}
        </svg>
      {/if}
    </div>{/if}
  <div class="track" class:full={trackOnly} bind:clientHeight={trackHeight}>
    {#if graph}
      <svg {width} height={trackH} aria-hidden="true">
        {#if graph.baselineY !== null}<line
            class="reply-median"
            x1="0"
            x2={width}
            y1={graph.baselineY}
            y2={graph.baselineY}
          />{/if}
        <!-- A bar spans its bucket's fastest to slowest reply, at least a dot's length, on a whole pixel. -->
        {#each graph.dots as dot, index (index)}
          {@const x = Math.round(dot.x)}
          <line
            class="reply"
            x1={x}
            x2={x}
            y1={dot.yLo}
            y2={Math.min(dot.yHi, dot.yLo - 1.5)}
          />
          {#if dot.over}<path
              class="reply-over"
              d={`M ${x - 2.5} 5.5 L ${x} 0.5 L ${x + 2.5} 5.5 Z`}
            />{/if}
        {/each}
        {#if hover}<line
            class="cursor"
            x1={hover.x}
            x2={hover.x}
            y1="0"
            y2={trackH}
          />{/if}
      </svg>
    {/if}
  </div>
  {#if hover}
    <!-- In the top layer, over the card's quiet line: centred on the cursor, inside the strip, off the graph. -->
    {@const at = box!.getBoundingClientRect()}
    <span
      class="inspect-card readout"
      class:unmeasured={!readoutWidth}
      popover="manual"
      {@attach (node) => node.showPopover()}
      bind:offsetWidth={readoutWidth}
      style:max-width="{width}px"
      style:left="{Math.round(
        at.left +
          Math.max(
            0,
            Math.min(hover.x - readoutWidth / 2, width - readoutWidth),
          ),
      )}px"
      style:top="{at.top}px"
    >
      <small>{hover.time}</small>
      {#each hover.rows as row (row.label)}
        <span
          >{#if row.mark}<small>{row.mark}</small>
          {/if}{row.value}</span
        >
      {/each}
    </span>
  {/if}
</div>

<style>
  .graph {
    position: relative;
    display: grid;
    grid-template-rows: minmax(0, 1fr) auto;
    gap: 6px;
    height: 100%;
    min-height: 0;
    border-radius: var(--r-well);
    cursor: crosshair;
    outline-offset: 4px;
    touch-action: pan-y pinch-zoom;
  }
  .graph[tabindex="-1"] {
    cursor: default;
  }
  .plot,
  .track {
    position: relative;
    min-height: 0;
  }
  .track {
    height: 20px;
  }
  .graph:has(> .track.full) {
    grid-template-rows: minmax(0, 1fr);
  }
  .track.full {
    height: 100%;
  }
  svg {
    position: absolute;
    inset: 0;
    overflow: visible;
  }
  /* Hairlines snap to device pixels instead of splitting across two rows. */
  .axis,
  .cursor {
    shape-rendering: crispEdges;
  }
  .axis {
    stroke: var(--border);
    stroke-width: 1;
  }
  .line {
    fill: none;
    stroke: var(--tone);
    stroke-width: 1.6;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .line.second {
    stroke-dasharray: 3 3;
  }
  .head {
    fill: var(--tone);
  }
  .cursor {
    stroke: color-mix(in oklab, var(--text) 45%, transparent);
    stroke-width: 1;
  }
  /* Whole pixels, so the plate's hairlines stay crisp wherever it stands. */
  .readout {
    position: fixed;
    inset: auto;
    margin: 0;
    translate: 0 round(calc(-100% - 4px), 1px);
  }
  .readout.unmeasured {
    visibility: hidden;
  }
</style>
