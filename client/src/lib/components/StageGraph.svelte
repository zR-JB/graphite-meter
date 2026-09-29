<script lang="ts">
  import { untrack } from "svelte";
  import { warmUp } from "../actions/intent";
  import { scrub } from "../actions/scrub";
  import { fmtDuration, formatLatency } from "../format";
  import {
    nearestAt,
    stageGraph,
    type GraphPoint,
    type LatencyPoint,
  } from "../presentation/stageGraph";

  interface Props {
    tone: "download" | "upload" | "bidirectional";
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
  const graph = $derived(
    width && plotHeight
      ? stageGraph({
          lanes,
          latency,
          start,
          span,
          ceiling,
          baseline,
          latencyTop,
          width,
          plotHeight,
          trackHeight: TRACK,
          head,
        })
      : null,
  );
  const hasData = $derived(!!graph?.bins.some((lane) => lane.length));

  let hoverT = $state<number | null>(null);
  let keyboard = false;
  // The readout reads measured samples; bins only draw, and their width changes with the card's.
  const samples = $derived(
    lanes.map((lane) => lane.filter((point) => point.t >= start)),
  );
  const hover = $derived.by(() => {
    if (hoverT === null || !graph || !hasData) return null;
    const rates = samples.map((lane) => nearestAt(lane, hoverT!));
    const at = rates.find(Boolean)!.t;
    const reply = nearestAt(latency, at);
    const near = reply && Math.abs(reply.t - at) <= span / 12 ? reply : null;
    const rows = rates.map((point, lane) => ({
      label: laneNames[lane] ?? "Rate",
      value: point ? rate(point.v) : "",
    }));
    if (near) rows.push({ label: "Latency", value: formatLatency(near.ms) });
    return {
      x: Math.min(width, Math.max(0, ((at - start) / (span || 1)) * width)),
      rows,
      time: fmtDuration(Math.max(0, at - start), 1),
    };
  });

  let box: HTMLElement | undefined = $state();
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
    const times = graph?.bins.find((lane) => lane.length)?.map((p) => p.t);
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
  <div class="plot" bind:clientHeight={plotHeight}>
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
  </div>
  <div class="track">
    {#if graph}
      <svg {width} height={TRACK} aria-hidden="true">
        {#if graph.baselineY !== null}<line
            class="reply-median"
            x1="0"
            x2={width}
            y1={graph.baselineY}
            y2={graph.baselineY}
          />{/if}
        {#each graph.dots as dot, index (index)}
          <circle class="reply" cx={dot.x} cy={dot.y} r="1.6" />
        {/each}
        {#if hover}<line
            class="cursor"
            x1={hover.x}
            x2={hover.x}
            y1="0"
            y2={TRACK}
          />{/if}
      </svg>
    {/if}
  </div>
  {#if hover}
    <span
      class="inspect-card readout"
      class:flip={hover.x > width / 2}
      style:left="{hover.x}px"
    >
      <span class="readout-time">{hover.time}</span>
      {#each hover.rows as row (row.label)}
        <span class="inspect-row"
          ><span>{row.label}</span><span>{row.value}</span></span
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
  .readout {
    z-index: 5;
    top: 0;
    min-width: 150px;
    white-space: nowrap;
    translate: 10px 0;
  }
  /* Whole pixels, so the card's hairlines stay crisp wherever it flips. */
  .readout.flip {
    translate: round(calc(-100% - 10px), 1px) 0;
  }
  .readout-time {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-xs) / 1.3 var(--font-sans);
  }
</style>
