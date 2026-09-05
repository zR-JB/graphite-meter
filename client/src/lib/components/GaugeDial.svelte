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
  import { tooltip } from "../actions/tooltip";
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

  // CSS owns interpolation; only suppress motion when this instrument is unseen.
  function attach(node: HTMLDivElement) {
    let intersecting = true;
    const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => {
      motion = intersecting && !document.hidden && !reducedMotion.matches;
    };
    const observer = new IntersectionObserver(([entry]) => {
      intersecting = entry.isIntersecting;
      update();
    });
    observer.observe(node);
    reducedMotion.addEventListener("change", update);
    document.addEventListener("visibilitychange", update);
    update();
    return () => {
      observer.disconnect();
      reducedMotion.removeEventListener("change", update);
      node
        .getAnimations({ subtree: true })
        .forEach((animation) => animation.cancel());
      document.removeEventListener("visibilitychange", update);
    };
  }
  let surface = $state<HTMLDivElement>();
  const extent = $derived(layout.radius + layout.arcWidth / 2 + 1);
  const diameter = $derived(extent * 2);
  let flight: { from: number; to: number; animation?: Animation } | undefined;
  const ease = (progress: number) => 1 - (1 - progress) ** 3;
  $effect(() => {
    const node = surface;
    const next = target * 270;
    const visible = input.showValue && !completed;
    const animate = motion && visible;
    if (!node) return;
    const current = flight
      ? flight.from +
        (flight.to - flight.from) *
          ease(Math.min(1, Number(flight.animation?.currentTime ?? 600) / 600))
      : next;
    flight = { from: animate ? current : next, to: next };
    const rotors = node.querySelectorAll<HTMLElement>(".rotor, .live-head");
    const rotation = (angle: number, index: number) =>
      index === 0
        ? Math.min(180, angle)
        : index === 1
          ? Math.max(0, angle - 180)
          : angle + 135;
    // The compositor follows one sampled ease for all three surfaces. Include
    // the exact half-ring crossing so both clips meet without a gap.
    const offsets = Array.from({ length: 31 }, (_, i) => i / 30);
    const crossing = (180 - current) / (next - current);
    if (crossing > 0 && crossing < 1) offsets.push(1 - Math.cbrt(1 - crossing));
    offsets.sort((a, b) => a - b);
    rotors.forEach((rotor, index) => {
      rotor.getAnimations().forEach((animation) => animation.cancel());
      rotor.style.transform = `rotate(${rotation(next, index)}deg)`;
      if (!animate || Math.abs(current - next) < 0.01) return;
      const animation = rotor.animate(
        offsets.map((offset) => ({
          offset,
          transform: `rotate(${rotation(current + (next - current) * ease(offset), index)}deg)`,
        })),
        { duration: 600, easing: "linear" },
      );
      if (index === 2) flight!.animation = animation;
    });
  });
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
  {@attach attach}
  bind:this={surface}
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
        <stop offset=".64" stop-color="rgba(var(--shadow-ink), .03)" />
        <stop offset="1" stop-color="rgba(var(--shadow-ink), .08)" />
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
          style:--head-color={`var(--phase-${result.phase})`}
          use:tooltip={{ text: result.description, instant: true }}
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
          >
            <svg
              width={diameter}
              height={diameter}
              viewBox={`0 0 ${diameter} ${diameter}`}
            >
              <path
                d={`M ${extent} ${extent - layout.radius} A ${layout.radius} ${layout.radius} 0 0 ${half} ${extent} ${extent + layout.radius}`}
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
    padding: 0;
    border: 0;
    border-radius: var(--r-full);
    background: transparent;
    transform: translate(-50%, -50%);
    cursor: help;
  }
  .result-head-target:hover {
    box-shadow: inset 0 0 0 1px var(--head-color);
  }
  .result-head-target:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 1px;
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
    transition: opacity 180ms ease-out;
  }
  .motion .live svg path {
    transition: stroke 180ms linear;
  }
  .motion .live svg circle {
    transition: fill 180ms linear;
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
