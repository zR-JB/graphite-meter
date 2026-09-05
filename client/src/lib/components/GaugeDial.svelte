<script module lang="ts">
  import type { SweepTargetInput } from "./gaugeSweep";
  import type { ResultArcPhase } from "./resultGauge";
  export interface GaugeDialState extends SweepTargetInput {
    showValue: boolean;
    resultArcs: readonly {
      phase: ResultArcPhase;
      fraction: number;
      dashed: boolean;
    }[];
  }
</script>

<script lang="ts">
  import { sweepTarget, angleForFraction } from "./gaugeSweep";
  import type { GaugeLayout } from "./gaugeLayout";
  import { resultGaugeHeadPlacements } from "./resultGauge";

  let { input, layout }: { input: GaugeDialState; layout: GaugeLayout } =
    $props();
  const shadeId = $props.id();
  let motion = $state(true);
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

  // CSS owns interpolation; only suppress motion when this instrument is unseen.
  function attach(node: SVGSVGElement) {
    let intersecting = true;
    const update = () => {
      motion = intersecting && !document.hidden;
    };
    const observer = new IntersectionObserver(([entry]) => {
      intersecting = entry.isIntersecting;
      update();
    });
    observer.observe(node);
    document.addEventListener("visibilitychange", update);
    update();
    return () => {
      observer.disconnect();
      document.removeEventListener("visibilitychange", update);
    };
  }
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
  result = false,
)}
  <g transform={`translate(${layout.center.x} ${layout.center.y})`}>
    <g
      class="head"
      class:result
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

<svg
  {@attach attach}
  class="gauge-dial"
  class:motion
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
      <stop offset=".64" stop-color="rgba(var(--shadow-ink), .03)" />
      <stop offset="1" stop-color="rgba(var(--shadow-ink), .08)" />
    </radialGradient>
  </defs>
  <g fill="none" stroke-linecap="round">
    <path d={track} stroke="var(--surface-2)" stroke-width={layout.arcWidth} />
    <g stroke="var(--border-strong)" stroke-width="1" opacity=".7">
      {#each layout.majorTicks as tick (tick.angle)}
        <path
          d={`M ${tick.from.x} ${tick.from.y} L ${tick.to.x} ${tick.to.y}`}
        />
      {/each}
    </g>
    <path
      class="sweep live"
      class:visible={input.showValue && !completed}
      d={track}
      pathLength="1"
      style:stroke-dasharray={`${target} 1`}
      stroke={accent}
      stroke-width={layout.arcWidth}
    />
    {#if completed}
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
            class="reveal"
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
    {/if}
  </g>
  {#if completed}
    {#each results.toReversed() as result (result.phase)}
      {@render head(
        result.fraction,
        result.radius,
        `var(--phase-${result.phase})`,
        result.dashed,
        result.lane,
        true,
      )}
    {/each}
  {/if}
  <g class="live" class:visible={input.showValue && !completed}>
    {@render head(target, layout.radius, accent)}
  </g>
</svg>

<style>
  .live {
    opacity: 0;
  }
  .live.visible {
    opacity: 1;
  }
  .motion .live {
    transition: opacity 180ms ease-out;
  }
  .motion .sweep {
    transition:
      stroke-dasharray 600ms cubic-bezier(0.16, 1, 0.3, 1),
      stroke 180ms linear,
      opacity 180ms ease-out;
  }
  .motion .head {
    transition: transform 600ms cubic-bezier(0.16, 1, 0.3, 1);
  }
  .motion .live circle {
    transition: fill 180ms linear;
  }
  .motion .live:not(.visible),
  .motion .live:not(.visible) * {
    transition: none;
  }
  .motion .reveal {
    transition: stroke-dasharray 550ms ease-out;
  }
  .motion .head.result {
    transition: transform 550ms ease-out;
  }
  @starting-style {
    .reveal {
      stroke-dasharray: 0 1 !important;
    }
    .head.result {
      transform: rotate(135deg) !important;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .motion .sweep,
    .motion .head,
    .motion .reveal,
    .motion .live,
    .motion .live circle {
      transition: none;
    }
  }
  .gauge-dial {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
</style>
