<script module lang="ts">
  import type { SweepTargetInput } from "./gaugeSweep";
  import type { ResultArcPhase } from "./resultGauge";
  export interface GaugeDialState extends SweepTargetInput {
    showValue: boolean;
    /** The latest idle reply's time while the latency stage runs; the head beats on each new one. */
    reply?: number | null;
    /** The stage the run is in, warmup included; the ring takes its hue. */
    stage?: string | null;
  }
  interface ResultArc {
    phase: ResultArcPhase;
    fraction: number;
    dashed: boolean;
    /** The headline result fills the arc; every result marks the rim. */
    primary: boolean;
    description: string;
  }
  /** The result sweep's length, as in the stylesheet's `result-sweep`. */
  const SWEEP_MS = 900;
  /** A stage's needle rises from zero and drains back to it over these. */
  const RISE_MS = 520;
  const DRAIN_MS = 300;
  /** A result's arcs drain back to zero over this as the next run starts, as in `result-drain`. */
  export const RESULT_DRAIN_MS = 300;
</script>

<script lang="ts">
  import { inView } from "../actions/inView";
  import { tooltip } from "../actions/tooltip";
  import { onDestroy, untrack } from "svelte";
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
    result: { arcs: readonly ResultArc[]; out: boolean };
  } = $props();
  const shadeId = $props.id();
  // An unseen dial snaps rather than animating.
  let seen = $state(true);
  const motion = $derived(seen && !still());
  const target = $derived(sweepTarget(input));
  const visible = $derived(input.showValue && target !== null);
  // A head is flat in its hue and a little wider than its arc, so it and its beat read at any size.
  const headRadius = $derived(layout.arcWidth / 2 + 2);
  // The beat swells the head by a fifth; its box leaves that room.
  const headExtent = $derived(Math.ceil(headRadius * 1.2) + 1);
  // Readout frames change the handoff object while the result arcs stay put.
  const arcs = $derived(result.arcs);
  const placements = $derived(
    resultGaugeHeadPlacements(
      arcs.map((arc) => arc.fraction),
      {
        baseRadius: layout.radius,
        arcSweep: layout.arcSweep,
        headRadius,
        borderWidth: 0,
      },
    ),
  );
  const results = $derived(
    arcs.map((arc, index) => ({
      ...arc,
      ...placements[index],
      fraction: Math.min(1, Math.max(0, arc.fraction)),
    })),
  );
  const extent = $derived(layout.radius + layout.arcWidth / 2 + 1);
  const diameter = $derived(extent * 2);
  // The needle follows the readout and glides across a rescale. A stage's first value rises from zero, and when
  // its value ends the needle drains back to zero in its own hue, so one stage hands the ring to the next through
  // zero rather than through a fade.
  const sweep = new Smoothed();
  onDestroy(() => sweep.dispose());
  let accent = $state("var(--phase-latency)");
  let course = "";
  let revealed = false;
  $effect(() => {
    const next = (target ?? 0) * 270;
    const current = `${input.scaleBytesPerSec}:${input.latencyScaleMs}`;
    // Between runs the needle keeps the hue of the stage it last showed.
    const hue =
      input.stage ??
      (["latency", "download", "upload", "bidirectional"].includes(input.phase)
        ? input.phase
        : null);
    untrack(() => {
      if (hue) accent = `var(--phase-${hue})`;
      if (!visible) {
        if (revealed)
          sweep.set(
            0,
            motion ? { over: DRAIN_MS, ease: true } : { snap: true },
          );
        revealed = false;
        return;
      }
      if (!revealed && motion && sweep.current < 0.5)
        sweep.set(next, { over: RISE_MS, ease: true });
      else
        sweep.set(
          next,
          !motion || !revealed
            ? { snap: true }
            : current === course
              ? { finish: true }
              : { over: 360, ease: true },
        );
      revealed = true;
      course = current;
    });
  });
  // A draining needle stays on the ring until it reaches zero.
  const lit = $derived(visible || (motion && sweep.current > 0.5));
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

<!-- Every result's arc lies on the ring, the longest underneath, so each shows from where the next shorter one
     ends; its head is a bead in its hue at its arc's end. One moved inward off a close neighbour hangs on a stalk
     of its hue, and a partial one is a ring. -->
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
        class="bead"
        style:--at="{(1 - Math.cbrt(1 - fraction)) * SWEEP_MS + DRAIN_MS}ms"
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
  style:--ring={input.stage ? `var(--phase-${input.stage})` : null}
  style:--drain="{DRAIN_MS}ms"
>
  <svg
    class="dial-art"
    aria-hidden="true"
    width={layout.width}
    height={layout.height}
    viewBox={`0 0 ${layout.width} ${layout.height}`}
  >
    <g fill="none" stroke-linecap="round">
      <path class="track" d={track} stroke-width={layout.arcWidth} />
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
    <div class="result-layer handoff" class:handoff-out={result.out}>
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
                style:--fraction={result.fraction}
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
  <div class="live" class:visible={lit} aria-hidden="true">
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
            style:transform={`rotate(${half === 0 ? Math.min(180, sweep.current) : Math.max(0, sweep.current - 180)}deg)`}
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
      style:transform={`rotate(${sweep.current + 135}deg)`}
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
            <circle
              class="ripple"
              r={headRadius}
              fill="none"
              stroke={accent}
              onanimationend={() => (beating = false)}
            />
          {/if}
          <circle r={headRadius} fill={accent} />
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
  /* The ring's track takes a faint tint of the running stage, from its warmup on; the needle's hue follows the
     stage too, so a draining needle blends into the next stage's hue on its way down. */
  .track {
    stroke: color-mix(
      in oklab,
      var(--ring, var(--border-strong)) 22%,
      var(--border-strong)
    );
  }
  .motion .track {
    transition: stroke 700ms var(--ease-out);
  }
  .motion .live :is(path, circle) {
    transition:
      stroke var(--dur-stage) var(--ease-out),
      fill var(--dur-stage) var(--ease-out);
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
  }
  .second .rotor {
    right: auto;
    left: 0;
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
  }
  .live-head svg {
    position: absolute;
    max-width: none;
    /* The ring widens past the head's box; the dial's own clip bounds it. */
    overflow: visible;
  }
  /* A reply rings out from the head: a faint hairline ring widens to twice the head and fades over one pulse,
     eased out, so a steady link is seen to answer while the head itself holds still. */
  /* The result replays the run as one sweep from zero once the last needle has drained (--drain): every arc
     shows up to the shared front, so the front changes hue as it passes each shorter result, and each bead lands
     on the spring as the front reaches it. Once, as the result arrives; the bead's moment follows the sweep's
     ease-out-cubic (SWEEP_MS). */
  /* A result arc shows up to the shared front; its own length and its bead glide when the scale or the unit
     moves them, as the needle does, so a unit switch never replays the sweep. */
  .result-arc {
    stroke-dasharray: min(var(--fraction), var(--sweep)) 1;
  }
  @media (prefers-reduced-motion: no-preference) {
    .result-arc {
      transition: --fraction 360ms var(--ease-out);
      animation: result-sweep 900ms cubic-bezier(0.33, 1, 0.68, 1) var(--drain)
        backwards;
    }
    .head.result {
      transition: transform 360ms var(--ease-out);
    }
    .bead {
      transform-box: fill-box;
      transform-origin: center;
      animation: pop 400ms var(--ease-spring) var(--at) backwards;
    }
    /* Until its sweep begins a result's arcs are at zero, where their round caps would leave a dot. */
    .result-layer > svg {
      animation: await-sweep 0s linear var(--drain) backwards;
    }
    /* A new run rewinds the result: every arc drains back to zero together and the beads drop off, then the
       first stage rises from the empty ring (RESULT_DRAIN_MS). */
    .result-layer.handoff-out {
      opacity: 1;
    }
    .result-layer.handoff-out .result-arc {
      animation: result-drain 300ms var(--ease-out) forwards;
    }
    .result-layer.handoff-out .bead {
      animation: bead-out 200ms var(--ease-out) forwards;
    }
  }
  @keyframes result-sweep {
    from {
      --sweep: 0;
    }
    to {
      --sweep: 1;
    }
  }
  @keyframes await-sweep {
    from {
      visibility: hidden;
    }
  }
  @keyframes result-drain {
    from {
      --sweep: 1;
    }
    to {
      --sweep: 0;
    }
  }
  @keyframes bead-out {
    to {
      opacity: 0;
      scale: 0.3;
    }
  }
  .ripple {
    transform-box: fill-box;
    transform-origin: center;
    stroke-width: 1;
    opacity: 0;
    animation: ripple var(--dur-pulse) var(--ease-out);
  }
  @keyframes ripple {
    from {
      scale: 1;
      opacity: 0.32;
    }
    to {
      scale: 2.2;
      opacity: 0;
    }
  }
</style>
