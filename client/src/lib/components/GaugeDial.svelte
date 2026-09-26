<script module lang="ts">
  import type { SweepTargetInput } from "./gaugeSweep";
  import type { ResultArcPhase } from "./resultGauge";
  export interface GaugeDialState extends SweepTargetInput {
    showValue: boolean;
    resultArcs: readonly {
      phase: ResultArcPhase;
      fraction: number;
      dashed: boolean;
      description?: string;
    }[];
  }
</script>

<script lang="ts">
  import { inView } from "../actions/inView";
  import { untrack } from "svelte";
  import { Smoothed, still } from "../presentation/motion.svelte";
  import { tooltip } from "../actions/tooltip";
  import { sweepTarget, angleForFraction } from "./gaugeSweep";
  import type { GaugeLayout } from "./gaugeLayout";
  import { resultGaugeHeadPlacements } from "./resultGauge";

  let { input, layout }: { input: GaugeDialState; layout: GaugeLayout } =
    $props();
  const shadeId = $props.id();
  // An unseen dial snaps rather than animating.
  let seen = $state(true);
  const motion = $derived(seen && !still());
  const completed = $derived(
    input.phase === "complete" && input.resultArcs.length > 0,
  );
  const target = $derived(sweepTarget(input));
  const headRadius = $derived.by(() => {
    const radius = Math.min(7.5, layout.arcWidth * 0.48);
    const close = input.resultArcs.some((arc, index, arcs) =>
      arcs
        .slice(index + 1)
        .some(
          (other) =>
            Math.abs(arc.fraction - other.fraction) *
              layout.arcSweep *
              layout.radius <
            2 * radius + 5,
        ),
    );
    return close ? Math.min(4, radius) : radius;
  });
  const headExtent = $derived(
    Math.max(headRadius + 1.5, layout.arcWidth / 2 + 0.5),
  );
  const placements = $derived(
    resultGaugeHeadPlacements(
      input.resultArcs.map((arc) => arc.fraction),
      {
        baseRadius: layout.radius,
        arcSweep: layout.arcSweep,
        headRadius,
        borderWidth: 1.5,
      },
    ),
  );
  const results = $derived(
    input.resultArcs.map((arc, index) => ({
      ...arc,
      ...placements[index],
      fraction: Math.min(1, Math.max(0, arc.fraction)),
    })),
  );
  const accent = $derived(
    input.phase === "idle"
      ? "var(--text-soft)"
      : input.phase === "error" || input.phase === "aborted"
        ? "var(--err)"
        : `var(--phase-${input.phase === "connecting" ? "warmup" : input.phase})`,
  );

  const extent = $derived(layout.radius + layout.arcWidth / 2 + 1);
  const diameter = $derived(extent * 2);
  const sweep = new Smoothed();
  $effect(() => {
    const next = target * 270;
    const snap = !motion || !input.showValue || completed;
    untrack(() => sweep.set(next, { snap }));
  });
  // Each half ring turns through its own 180°, so both clips meet at the crossing.
  const halfAngle = (half: number) =>
    half ? Math.max(0, sweep.current - 180) : Math.min(180, sweep.current);
  const halfRing = (sweep: number) => {
    const r = layout.radius;
    return `M ${extent} ${extent - r} A ${r} ${r} 0 0 ${sweep} ${extent} ${extent + r}`;
  };
  const track = $derived.by(() => {
    const { center, radius, arcStart, arcSweep } = layout;
    const start = {
      x: center.x + Math.cos(arcStart) * radius,
      y: center.y + Math.sin(arcStart) * radius,
    };
    const end = {
      x: center.x + Math.cos(arcStart + arcSweep) * radius,
      y: center.y + Math.sin(arcStart + arcSweep) * radius,
    };
    return `M ${start.x} ${start.y} A ${radius} ${radius} 0 1 1 ${end.x} ${end.y}`;
  });
</script>

{#snippet head(
  fraction: number,
  radius: number,
  color: string,
  hollow = false,
  lane = 0,
)}
  <g transform={`translate(${layout.center.x} ${layout.center.y})`}>
    <g
      class="head result"
      style:transform={`rotate(${angleForFraction(fraction, layout.arcStart, layout.arcSweep)}rad)`}
    >
      {#if lane !== 0}
        <path
          d={`M ${layout.radius} 0 H ${radius}`}
          stroke="var(--surface-inset)"
          stroke-width="4"
        />
        <path
          d={`M ${layout.radius} 0 H ${radius}`}
          stroke={color}
          stroke-width="2"
        />
      {/if}
      <g transform={`translate(${radius} 0)`}>
        <circle
          r={headRadius + 0.75}
          fill={hollow ? "var(--surface-inset)" : color}
          stroke="var(--surface-inset)"
          stroke-width="1.5"
        />
        {#if hollow}<circle
            r={headRadius * 0.68}
            fill="none"
            stroke={color}
            stroke-width="1"
          />{/if}
      </g>
    </g>
  </g>
{/snippet}

<div
  {@attach inView((value) => (seen = value))}
  class="gauge-dial"
  class:motion
  role={completed ? "group" : undefined}
  aria-label={completed ? "Completed throughput measurements" : undefined}
>
  <svg
    class="dial-art"
    aria-hidden="true"
    width={layout.width}
    height={layout.height}
    viewBox={`0 0 ${layout.width} ${layout.height}`}
  >
    <defs>
      <radialGradient
        id={shadeId}
        gradientUnits="userSpaceOnUse"
        cx={layout.center.x}
        cy={layout.center.y}
        r={layout.radius + layout.arcWidth / 2}
        fr={layout.radius - layout.arcWidth / 2}
      >
        <stop offset="0" stop-color="var(--edge-highlight)" />
        <stop
          offset=".38"
          stop-color="color-mix(in srgb, var(--edge-highlight) 40%, transparent)"
        />
        <stop
          offset=".5"
          stop-color="color-mix(in srgb, var(--edge-highlight) 80%, transparent)"
        />
        <stop
          offset=".64"
          stop-color="color-mix(in srgb, var(--shade) 3%, transparent)"
        />
        <stop
          offset="1"
          stop-color="color-mix(in srgb, var(--shade) 8%, transparent)"
        />
      </radialGradient>
    </defs>
    <g fill="none" stroke-linecap="round">
      <path
        d={track}
        stroke="var(--surface-2)"
        stroke-width={layout.arcWidth}
      />
      <g stroke="var(--border-strong)" stroke-width="1" opacity=".7">
        {#each layout.majorTicks as tick (tick.angle)}
          <path
            d={`M ${tick.from.x} ${tick.from.y} L ${tick.to.x} ${tick.to.y}`}
          />
        {/each}
      </g>
    </g>
  </svg>
  {#if completed}
    <div class="result-layer">
      <svg
        class="dial-art"
        aria-hidden="true"
        width={layout.width}
        height={layout.height}
        viewBox={`0 0 ${layout.width} ${layout.height}`}
      >
        <g fill="none" stroke-linecap="round">
          {#each results as result (result.phase)}
            <mask
              id={`${shadeId}-${result.phase}`}
              maskUnits="userSpaceOnUse"
              x="0"
              y="0"
              width={layout.width}
              height={layout.height}
            >
              <path
                class="result-arc"
                d={track}
                pathLength="1"
                style:stroke-dasharray={`${result.fraction} 1`}
                stroke="white"
                stroke-width={layout.arcWidth + 2}
              />
            </mask>
            <g
              mask={`url(#${shadeId}-${result.phase})`}
              stroke-width={layout.arcWidth}
              stroke-dasharray={result.dashed
                ? `${layout.arcWidth * 1.5} ${layout.arcWidth}`
                : undefined}
            >
              <path d={track} stroke={`var(--phase-${result.phase})`} />
              <path d={track} stroke={`url(#${shadeId})`} />
            </g>
          {/each}
        </g>
        {#each results.toReversed() as result (result.phase)}
          {@render head(
            result.fraction,
            result.radius,
            `var(--phase-${result.phase})`,
            result.dashed,
            result.lane,
          )}
        {/each}
      </svg>
    </div>
  {/if}
  {#if completed}
    {#each results as result (result.phase)}
      {#if result.description}
        {@const angle = angleForFraction(
          result.fraction,
          layout.arcStart,
          layout.arcSweep,
        )}
        <span
          class="result-head-target"
          role="img"
          aria-label={result.description}
          style:left={`${layout.center.x + Math.cos(angle) * result.radius}px`}
          style:top={`${layout.center.y + Math.sin(angle) * result.radius}px`}
          {@attach tooltip(() => ({
            text: result.description ?? "",
            instant: true,
          }))}
        ></span>
      {/if}
    {/each}
  {/if}
  <div
    class="live"
    class:visible={input.showValue && !completed}
    aria-hidden="true"
  >
    <div
      class="sweep-ring"
      style:left={`${layout.center.x - extent}px`}
      style:top={`${layout.center.y - extent}px`}
      style:width={`${diameter}px`}
      style:height={`${diameter}px`}
    >
      {#each [0, 1] as half (half)}
        <div class="half-clip" class:second={half === 1}>
          <div
            class="rotor"
            style:width={`${diameter}px`}
            style:height={`${diameter}px`}
            style:transform={`rotate(${halfAngle(half)}deg)`}
          >
            <svg
              width={diameter}
              height={diameter}
              viewBox={`0 0 ${diameter} ${diameter}`}
            >
              <path
                d={halfRing(half)}
                fill="none"
                stroke={accent}
                stroke-width={layout.arcWidth}
              />
            </svg>
          </div>
        </div>
      {/each}
      <svg
        class="start-cap"
        width={diameter}
        height={diameter}
        viewBox={`0 0 ${diameter} ${diameter}`}
      >
        <circle
          cx={extent}
          cy={extent - layout.radius}
          r={layout.arcWidth / 2}
          fill={accent}
        />
      </svg>
    </div>
    <div
      class="live-head"
      style:left={`${layout.center.x}px`}
      style:top={`${layout.center.y}px`}
      style:transform={`rotate(${sweep.current + 135}deg)`}
    >
      <svg
        style:left={`${layout.radius - headExtent}px`}
        style:top={`${-headExtent}px`}
        width={headExtent * 2}
        height={headExtent * 2}
        viewBox={`${-headExtent} ${-headExtent} ${headExtent * 2} ${headExtent * 2}`}
      >
        <circle class="sweep-end-cap" r={layout.arcWidth / 2} fill={accent} />
        <circle
          r={headRadius + 0.75}
          fill={accent}
          stroke="var(--surface-inset)"
          stroke-width="1.5"
        />
      </svg>
    </div>
  </div>
</div>

<style>
  .result-head-target {
    position: absolute;
    z-index: 1;
    width: 24px;
    height: 24px;
    transform: translate(-50%, -50%);
    cursor: help;
  }

  .gauge-dial,
  .dial-art,
  .live,
  .result-layer {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  .live {
    opacity: 0;
    pointer-events: none;
  }
  .live.visible {
    opacity: 1;
  }
  .motion .live,
  .motion .result-layer {
    transition: opacity var(--dur-slide) var(--ease-out);
  }
  .motion .live svg path {
    transition: stroke var(--dur-slide) linear;
  }
  .motion .live svg circle {
    transition: fill var(--dur-slide) linear;
  }
  @starting-style {
    .motion .result-layer {
      opacity: 0;
    }
  }
  .sweep-ring {
    position: absolute;
    transform: rotate(225deg);
  }
  .half-clip {
    position: absolute;
    right: 0;
    top: 0;
    width: 50%;
    height: 100%;
    overflow: hidden;
  }
  .half-clip.second {
    right: auto;
    left: 0;
  }
  .rotor {
    position: absolute;
    top: 0;
    right: 0;
  }
  .second .rotor {
    right: auto;
    left: 0;
  }
  .start-cap {
    position: absolute;
    inset: 0;
  }
  .live-head {
    position: absolute;
    width: 0;
    height: 0;
  }
  .live-head svg {
    position: absolute;
    max-width: none;
  }
</style>
