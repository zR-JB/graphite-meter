<script module lang="ts">
  import type { SweepTargetInput } from "./gaugeSweep";
  import type { ResultArcPhase } from "./resultGauge";
  export interface GaugeDialState extends SweepTargetInput {
    showValue: boolean;
    /** The latest idle reply's time while the latency stage runs; the head beats on each new one. */
    reply?: number | null;
  }
  interface ResultArc {
    phase: ResultArcPhase;
    fraction: number;
    dashed: boolean;
    description: string;
  }
</script>

<script lang="ts">
  import { inView } from "../actions/inView";
  import { tooltip } from "../actions/tooltip";
  import { untrack } from "svelte";
  import { Smoothed, still } from "../presentation/motion.svelte";
  import { sweepTarget, angleForFraction } from "./gaugeSweep";
  import type { GaugeLayout } from "./gaugeLayout";
  import { resultGaugeHeadPlacements } from "./resultGauge";

  let {
    input,
    layout,
    result,
  }: {
    input: GaugeDialState;
    layout: GaugeLayout;
    result: { arcs: readonly ResultArc[]; opacity: number };
  } = $props();
  const shadeId = $props.id();
  // An unseen dial snaps rather than animating.
  let seen = $state(true);
  const motion = $derived(seen && !still());
  const target = $derived(sweepTarget(input));
  const visible = $derived(input.showValue && target !== null);
  // A head is flat in its hue and a little wider than its arc, so it and its beat read at any size.
  const headRadius = $derived(
    layout.arcWidth / 2 + Math.max(2, layout.arcWidth * 0.1),
  );
  // The beat swells the head by a fifth; its box leaves that room.
  const headExtent = $derived(Math.ceil(headRadius * 1.2) + 1);
  const placements = $derived(
    resultGaugeHeadPlacements(
      result.arcs.map((arc) => arc.fraction),
      {
        baseRadius: layout.radius,
        arcSweep: layout.arcSweep,
        headRadius,
        borderWidth: 0,
      },
    ),
  );
  const results = $derived(
    result.arcs.map((arc, index) => ({
      ...arc,
      ...placements[index],
      fraction: Math.min(1, Math.max(0, arc.fraction)),
    })),
  );
  const extent = $derived(layout.radius + layout.arcWidth / 2 + 1);
  const diameter = $derived(extent * 2);
  // The needle follows the readout, glides across a rescale, holds its pose while hidden and is revealed at the value.
  const sweep = new Smoothed();
  let accent = $state("var(--phase-latency)");
  let course = "";
  let revealed = false;
  $effect(() => {
    const next = (target ?? 0) * 270;
    const current = `${input.scaleBytesPerSec}:${input.latencyScaleMs}`;
    const tone = `var(--phase-${input.phase})`;
    if (!visible) revealed = false;
    else
      untrack(() => {
        sweep.set(
          next,
          !motion || !revealed
            ? { snap: true }
            : current === course
              ? { finish: true }
              : { over: 480 },
        );
        accent = tone;
        revealed = true;
        course = current;
      });
  });
  // One pulse at a time, each started by a reply: a steady link beats, a stalled one holds still.
  let beat = $state<number | null>(null);
  let beating = false;
  $effect(() => {
    const reply = input.reply;
    untrack(() => {
      if (reply == null || reply === beat || beating || !motion) return;
      beat = reply;
      beating = true;
    });
  });
  const capUnderHead = $derived(
    (sweep.current * Math.PI * layout.radius) / 180 <
      headRadius + layout.arcWidth / 2,
  );
  // Each half ring turns through its own 180°, so both clips meet at the crossing.
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

<!-- Flat in the stage's hue; a head moved inward off a close neighbour hangs on a stalk of its hue, and a partial one is a ring. -->
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
          stroke={color}
          stroke-width="2"
        />
      {/if}
      <circle
        cx={radius}
        r={hollow ? headRadius - 1 : headRadius}
        fill={hollow ? "none" : color}
        stroke={hollow ? color : undefined}
        stroke-width={hollow ? 2 : undefined}
      />
    </g>
  </g>
{/snippet}

<div
  {@attach inView((value) => (seen = value))}
  class="gauge-dial"
  class:motion
>
  <svg
    class="dial-art"
    aria-hidden="true"
    width={layout.width}
    height={layout.height}
    viewBox={`0 0 ${layout.width} ${layout.height}`}
  >
    <g fill="none" stroke-linecap="round">
      <path d={track} stroke="var(--border)" stroke-width={layout.arcWidth} />
      <g stroke="var(--border-strong)" stroke-width="1" stroke-opacity=".7">
        {#each layout.majorTicks as tick (tick.angle)}
          <path
            d={`M ${tick.from.x} ${tick.from.y} L ${tick.to.x} ${tick.to.y}`}
          />
        {/each}
      </g>
    </g>
  </svg>
  {#if results.length}
    <div class="result-layer" style:opacity={result.opacity}>
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
            <!-- Round caps add an arc width to every dash, so a partial arc's gap stays open. -->
            <path
              d={track}
              mask={`url(#${shadeId}-${result.phase})`}
              stroke={`var(--phase-${result.phase})`}
              stroke-width={layout.arcWidth}
              stroke-dasharray={result.dashed
                ? `${layout.arcWidth * 0.5} ${layout.arcWidth * 2}`
                : undefined}
            />
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
    {#each results as result (result.phase)}
      {@const angle = angleForFraction(
        result.fraction,
        layout.arcStart,
        layout.arcSweep,
      )}
      <!-- Pointer-only: the cards and the announcement carry these values. -->
      <span
        class="head-target"
        aria-hidden="true"
        tabindex="-1"
        style:left={`${layout.center.x + Math.cos(angle) * result.radius}px`}
        style:top={`${layout.center.y + Math.sin(angle) * result.radius}px`}
        {@attach tooltip(() => result.description)}
      ></span>
    {/each}
  {/if}
  <div
    class="live"
    class:visible
    style:--sweep={`${sweep.current}deg`}
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
        <!-- At rest against its clip edge, a half would bleed a hairline at the seam; the head covers the first degree. -->
        <div
          class="half-clip"
          class:second={half === 1}
          hidden={sweep.current <= (half === 1 ? 180 : 1)}
        >
          <div
            class="rotor"
            style:width={`${diameter}px`}
            style:height={`${diameter}px`}
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
        style:visibility={capUnderHead ? "hidden" : null}
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
    >
      <svg
        style:left={`${layout.radius - headExtent}px`}
        style:top={`${-headExtent}px`}
        width={headExtent * 2}
        height={headExtent * 2}
        viewBox={`${-headExtent} ${-headExtent} ${headExtent * 2} ${headExtent * 2}`}
      >
        {#key beat}
          <!-- A reply rings out from the head: a hairline ring in its hue widens and fades as the head settles. -->
          {#if beat !== null}
            <circle class="ripple" r={headRadius} fill="none" stroke={accent} />
          {/if}
          <circle
            class:beat={beat !== null}
            r={headRadius}
            fill={accent}
            onanimationend={() => (beating = false)}
          />
        {/key}
      </svg>
    </div>
  </div>
</div>

<style>
  .gauge-dial,
  .dial-art,
  .live,
  .result-layer {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  /* The ring turns as a square; its empty corners must not widen a phone's page. */
  .live {
    opacity: 0;
    pointer-events: none;
    overflow: clip;
  }
  .live.visible {
    opacity: 1;
  }
  .motion .live {
    transition: opacity var(--dur-slide) var(--ease-out);
  }
  .motion .live svg path {
    transition: stroke var(--dur-slide) linear;
  }
  .motion .live svg circle {
    transition: fill var(--dur-slide) linear;
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
  /* One pixel of overlap, so the two halves never meet on an antialiased crack. */
  .half-clip.second {
    right: auto;
    left: 0;
    width: calc(50% + 1px);
  }
  .rotor {
    position: absolute;
    top: 0;
    right: 0;
    transform: rotate(min(180deg, var(--sweep)));
  }
  .second .rotor {
    right: auto;
    left: 0;
    transform: rotate(max(0deg, var(--sweep) - 180deg));
  }
  .start-cap {
    position: absolute;
    inset: 0;
  }
  .head-target {
    position: absolute;
    z-index: 1;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    translate: -50% -50%;
  }
  .live-head {
    position: absolute;
    width: 0;
    height: 0;
    transform: rotate(calc(var(--sweep) + 135deg));
  }
  .live-head svg {
    position: absolute;
    max-width: none;
    /* The ring widens past the head's box; the dial's own clip bounds it. */
    overflow: visible;
  }
  /* A reply's beat: the head swells and settles over one live pulse, and a ring spreads from it and fades,
     so a steady link is seen to answer even while the needle holds still. */
  .beat,
  .ripple {
    transform-box: fill-box;
    transform-origin: center;
  }
  .beat {
    animation: beat var(--dur-pulse) var(--ease-out);
  }
  .ripple {
    stroke-width: 1.5;
    opacity: 0;
    animation: ripple var(--dur-pulse) cubic-bezier(0.2, 0.6, 0.35, 1);
  }
  @keyframes beat {
    from {
      scale: 1.2;
    }
  }
  @keyframes ripple {
    from {
      scale: 1;
      opacity: 0.6;
    }
    to {
      scale: 3;
      opacity: 0;
    }
  }
</style>
