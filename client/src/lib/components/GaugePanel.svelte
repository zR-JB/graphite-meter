<script lang="ts">
  import { catalogSelection } from "../presentation/serverAppearance";
  import { untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import GaugeDial, { type GaugeDialState } from "./GaugeDial.svelte";
  import { GAUGE_LABEL_FRACTIONS, gaugeLayout } from "./gaugeLayout";
  import {
    fmtGaugeTick,
    throughputGaugeFraction,
    throughputValueAtFraction,
  } from "./gaugeScale";
  import StageTrack from "./StageTrack.svelte";
  import ServerLens from "./ServerLens.svelte";
  import RunButton from "./RunButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import ResultCards from "./ResultCards.svelte";
  import { fmtSpeed } from "../format";
  import { gaugeLatency as latencyGauge } from "../presentation/scales";
  import { LiveReadout, liveTargets } from "../presentation/liveReadout.svelte";
  import { primaryResultGaugeArc, resultGaugeArcs } from "./resultGauge";
  import { gaugeReadout } from "./gaugeReadout";
  import {
    MISSING,
    OUTCOME,
    PHASE_HINT,
    STAGE,
    STATUS_TONE,
  } from "../presentation/vocabulary";
  import { announceChanges } from "../presentation/announcer.svelte";
  import { tooltip } from "../actions/tooltip";
  import { handoff } from "../presentation/motion.svelte";

  const indicatedServers = $derived(
    store.serverDetails?.selection ??
      catalogSelection(store.serverCatalog, store.selectedServers),
  );
  // Who is in the run: the servers still measuring, counted like the result row.
  const participants = $derived(
    store.serverDetails
      ? indicatedServers.filter(({ id }) =>
          store.serverDetails!.participants.includes(id),
        )
      : indicatedServers,
  );
  const phase = $derived(store.phase);
  const activeStagePresentation = $derived(
    store.phaseStage ? store.stagePresentation[store.phaseStage] : null,
  );
  const terminalArcs = $derived(resultGaugeArcs(store.result));
  const headlineArc = $derived(primaryResultGaugeArc(terminalArcs));
  // A one-sided bidirectional partial has no truthful combined gauge value.
  const unusableStage = $derived(
    activeStagePresentation?.status === "failed" ||
      (phase === "complete" &&
        terminalArcs.length === 0 &&
        !store.result?.latency),
  );

  let gaugeWidth = $state(0);
  let gaugeHeight = $state(0);
  const liveReadout = new LiveReadout();
  $effect(() => {
    const live = store.live;
    const run = store.runSeq;
    untrack(() => liveReadout.update(live, run));
  });
  const liveRates = $derived(liveReadout.rates);

  const completedKind = $derived<"speed" | "latency">(
    terminalArcs.length ? "speed" : "latency",
  );
  const gaugeLatency = $derived.by(() => {
    return latencyGauge({
      phase,
      liveRttMs: store.liveRtt,
      axisMs: store.latencyScaleMs,
      history: store.latency,
      completedRttMs:
        phase === "complete" && terminalArcs.length === 0
          ? (store.result?.latency?.reportedMs ?? null)
          : null,
    });
  });

  const msTicksActive = $derived(
    phase === "latency" ||
      (phase === "complete" && completedKind === "latency"),
  );
  $effect(() => {
    const ms = gaugeLatency.rttMs;
    const shown = msTicksActive;
    untrack(() => liveReadout.rtt.set(ms, { snap: !shown }));
  });
  const gaugeScaleBytesPerSec = $derived(store.scales.gaugeBytesPerSec);
  const gaugeUnit = $derived(store.unitLabel);
  const gaugeRate = (bytesPerSec: number) => store.toUnit(bytesPerSec);
  const gaugeTicks = $derived.by(() => {
    if (msTicksActive)
      return GAUGE_LABEL_FRACTIONS.map((fraction) => ({
        fraction,
        label: fmtGaugeTick(gaugeLatency.scaleMs * fraction),
      }));
    return GAUGE_LABEL_FRACTIONS.map((fraction) => ({
      fraction,
      label: fmtGaugeTick(
        gaugeRate(throughputValueAtFraction(fraction, gaugeScaleBytesPerSec)),
      ),
    }));
  });
  const layout = $derived(gaugeLayout(gaugeWidth, gaugeHeight));
  const liveTarget = $derived(liveTargets(store.live));
  const ticks = handoff(
    () =>
      !unusableStage &&
      (phase === "latency" ||
        phase === "warmup" ||
        phase === "download" ||
        phase === "upload" ||
        phase === "bidirectional" ||
        phase === "complete")
        ? gaugeTicks.map((tick) => tick.label)
        : [],
    (labels) => labels.join(),
  );

  const readout = $derived(
    gaugeReadout({
      phase,
      running: store.isRunning,
      preparing: store.preparing,
      preparation: store.preparation,
      startError: store.startError || store.startBlocker,
      error: store.error,
      latencyTimeout: store.liveLatencyLost,
      latencyMs: liveReadout.rtt.current,
      hasLatencyResult: !!store.result?.latency,
      unusable: unusableStage,
      headline: headlineArc,
      rate: (bytesPerSec) => fmtSpeed(gaugeRate(bytesPerSec)),
      unit: gaugeUnit,
    }),
  );

  announceChanges(() => readout.announcement);
  const rateDisplay = (rates: typeof liveRates) =>
    readout.display ??
    (rates
      ? { value: fmtSpeed(gaugeRate(rates.down + rates.up)), unit: gaugeUnit }
      : { value: MISSING, unit: "" });
  const spoken = $derived(rateDisplay(liveTarget));
  const hero = handoff(
    () => {
      const scale = store.scales.gaugeBytesPerSec;
      return {
        terminal: readout.terminal,
        display: rateDisplay(liveRates),
        unit: gaugeUnit,
        arcs:
          phase === "complete"
            ? terminalArcs.map((arc) => ({
                phase: arc.phase,
                fraction: throughputGaugeFraction(arc.bytesPerSec, scale),
                dashed: arc.dashed,
                description: `${arc.label}${arc.dashed ? ` · ${OUTCOME.partial}` : ""}\n${fmtSpeed(gaugeRate(arc.bytesPerSec))} ${gaugeUnit}`,
              }))
            : [],
      };
    },
    ({ terminal, display }) =>
      terminal
        ? `${terminal.phase}:${terminal.value}`
        : `${display.value === MISSING}:${display.unit}`,
  );
  const { terminal, display } = $derived(hero.shown);
  const dialState = $derived<GaugeDialState>({
    phase,
    showValue: !unusableStage,
    valueBytesPerSec: liveRates ? liveRates.down + liveRates.up : 0,
    scaleBytesPerSec: store.scales.gaugeBytesPerSec,
    throughputEvidence: liveRates !== null,
    latencyScaleMs: gaugeLatency.scaleMs,
    rtt: liveReadout.rtt.current,
    completedKind,
  });
  const footer = handoff(
    () => {
      const { hint, status, failure } = readout;
      if (store.preparing)
        return { status: readout.preparationLabel, tone: "preparation" };
      if (failure)
        return {
          status: failure.headline,
          tone: "error",
          hint: failure.detail,
        };
      if (status)
        return {
          status: status.headline,
          tone: status.error ? "error" : "",
          hint: status.action,
        };
      const known = PHASE_HINT[phase];
      return hint ? { hint: known?.text ?? hint, tip: known?.tip } : {};
    },
    (notes: { status?: string; tone?: string; hint?: string; tip?: string }) =>
      `${notes.status}|${notes.hint}`,
  );
</script>

<section class="gauge-panel" data-phase={store.phase}>
  <div class="instrument">
    <div class="dial">
      {#if indicatedServers.length > 1}
        <div class="server-indicator">
          <ServerLens servers={indicatedServers} {participants} />
        </div>
      {/if}
      <div
        bind:clientWidth={gaugeWidth}
        bind:clientHeight={gaugeHeight}
        class="gauge-face"
        style:--gauge-center-offset={`${layout.center.y - layout.height / 2}px`}
      >
        <GaugeDial
          input={dialState}
          {layout}
          result={{ arcs: hero.shown.arcs, opacity: hero.opacity }}
        />
        {#if ticks.shown.length > 1}
          <div
            class="gauge-ticks"
            aria-hidden="true"
            style:opacity={ticks.opacity}
          >
            {#each layout.labelPoints as point, index (index)}
              <span
                class="gauge-tick"
                data-anchor-x={point.anchorX}
                data-anchor-y={point.anchorY}
                style:left={`${point.x}px`}
                style:top={`${point.y}px`}>{ticks.shown[index]}</span
              >
            {/each}
          </div>
        {/if}
        <div class="metric-wrap" style:opacity={hero.opacity}>
          <div class="hero" class:terminal={!!terminal}>
            {#if terminal}
              <div
                class="terminal-readout"
                class:partial={terminal.dashed}
                aria-hidden="true"
              >
                <span class="terminal-direction" data-tone={terminal.direction}>
                  <span class="terminal-dot"></span>
                  {STAGE[terminal.direction].label}
                </span>
                <span class="terminal-number">{terminal.value}</span>
                <span class="terminal-unit">{hero.shown.unit}</span>
                {#if terminal.dashed}
                  <span class="terminal-partial" data-tone={STATUS_TONE.partial}
                    >{OUTCOME.partial}</span
                  >
                {/if}
              </div>
            {:else}
              <span class="gauge-value" aria-hidden="true">{display.value}</span
              >
              {#if display.unit}<span class="gauge-unit" aria-hidden="true"
                  >{display.unit}</span
                >{/if}
            {/if}
            <span class="sr-only">{spoken.value} {spoken.unit}</span>
          </div>
        </div>
      </div>
      <div class="gauge-footer" style:opacity={footer.opacity}>
        {#if footer.shown.status || footer.shown.hint}
          {@const { status, tone, hint, tip } = footer.shown}
          <div class="gauge-notes">
            {#if status}
              <span class="gauge-status {tone}">{status}</span>
            {/if}
            {#if hint}
              <span class="gauge-hint" {@attach tip ? tooltip(() => tip) : null}
                >{hint}</span
              >
            {/if}
          </div>
        {/if}
      </div>
    </div>

    {#if store.latencyEnabled}
      <div class="latency-slot"><LatencyProfile /></div>
    {/if}

    <div class="run-bar">
      <StageTrack />
      <RunButton />
    </div>

    <div class="results"><ResultCards live={liveReadout} /></div>
  </div>
</section>

<style>
  .gauge-panel {
    container: viz / inline-size;
    height: 100%;
  }
  /* The dial and latency share the top; the run bar and the transfer cards keep their height, the top takes the rest. */
  .instrument {
    display: grid;
    height: 100%;
    gap: var(--space-4) var(--space-5);
    grid-template:
      "dial" minmax(280px, 42svh)
      "run" auto
      "results" auto
      "latency" auto
      / minmax(0, 1fr);
  }
  @container viz (min-width: 760px) {
    .instrument {
      grid-template:
        "dial latency" minmax(min-content, 1fr)
        "run run" auto
        "results results" auto
        / minmax(240px, 4fr) minmax(0, 8fr);
    }
    .instrument:not(:has(.latency-slot)) {
      grid-template:
        "dial" minmax(220px, 1fr)
        "run" auto
        "results" auto
        / minmax(0, 1fr);
    }
  }
  .dial {
    grid-area: dial;
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  /* Never shorter than its content: a tight screen scrolls rather than overlapping the run bar. */
  .latency-slot {
    grid-area: latency;
    display: grid;
    min-width: 0;
  }
  .results {
    grid-area: results;
    min-width: 0;
  }
  /* What runs next and the one action that runs it, on one line. */
  .run-bar {
    grid-area: run;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: var(--space-3) var(--space-4);
  }
  .server-indicator {
    display: flex;
    flex: none;
    align-items: center;
    gap: 6px;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.4 var(--font-sans);
  }
  .server-indicator :global(svg) {
    width: 14px;
    height: 14px;
  }
  .gauge-face {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
    /* The hero number scales with cqmin, the dimension that sizes the ring. */
    container-type: size;
  }
  .gauge-ticks,
  .metric-wrap {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .gauge-tick {
    --x: -50%;
    --y: -50%;
    position: absolute;
    translate: var(--x) var(--y);
    color: var(--text-soft);
    font: var(--w-normal) var(--type-2xs) / 1 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .gauge-tick[data-anchor-x="end"] {
    --x: -100%;
  }
  .gauge-tick[data-anchor-x="start"] {
    --x: 0;
  }
  .gauge-tick[data-anchor-y="end"] {
    --y: -100%;
  }
  .gauge-tick[data-anchor-y="start"] {
    --y: 0;
  }
  .metric-wrap {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding-inline: 9%;
    padding-top: calc(2 * var(--gauge-center-offset));
  }
  /* Tabular figures keep a live value from shifting layout; light weight reads as a measured value. */
  .gauge-value,
  .terminal-number {
    max-width: 100%;
    color: var(--text);
    font-family: var(--font-display);
    font-weight: 300;
    font-variant-numeric: lining-nums tabular-nums;
    letter-spacing: -0.03em;
    white-space: nowrap;
  }
  .gauge-value {
    min-width: 5ch;
    font-size: clamp(24px, 17cqmin, 76px);
    line-height: 0.95;
    text-align: center;
  }
  .hero {
    display: flex;
    flex-direction: column;
    align-items: center;
    max-width: 100%;
    border-radius: var(--r-chrome);
    pointer-events: auto;
  }
  .hero.terminal {
    max-width: 72%;
  }
  .terminal-readout {
    position: relative;
    display: grid;
    justify-items: center;
    gap: var(--space-1);
    max-width: 100%;
  }
  .terminal-direction {
    position: absolute;
    bottom: calc(100% + clamp(10px, 4cqmin, 16px));
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font-size: clamp(var(--type-xs), 3.6cqmin, 14px);
    font-weight: var(--w-strong);
    line-height: 1;
    white-space: nowrap;
  }
  @container (max-height: 180px) {
    .terminal-direction {
      display: none;
    }
  }
  .terminal-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  .terminal-number {
    font-size: clamp(30px, 17cqmin, 76px);
    line-height: 1;
  }
  /* Unit symbols are case-significant: Mbit/s, kB/s, MiB/s. */
  .terminal-unit,
  .gauge-unit {
    color: var(--text-muted);
    font-family: var(--font-sans);
    line-height: 1;
  }
  .terminal-unit {
    font-size: clamp(var(--type-sm), 4cqmin, var(--type-lg));
    font-weight: var(--w-normal);
  }
  .gauge-unit {
    margin-top: var(--space-1);
    font-size: var(--type-md);
    font-weight: var(--w-normal);
  }
  .terminal-partial {
    color: var(--tone);
    font-size: var(--type-xs);
  }
  /* A separate footer keeps notes off the dial; it holds two lines, so a longer note never shrinks the ring. */
  .gauge-footer {
    display: grid;
    align-items: center;
    min-height: calc(var(--space-2) + 2.7 * var(--type-sm));
    padding-top: var(--space-1);
  }
  .gauge-notes {
    display: grid;
    gap: var(--space-1);
    text-align: center;
  }
  .gauge-hint {
    color: var(--text-muted);
    font: var(--w-normal) var(--type-body) / 1.35 var(--font-sans);
  }
  .gauge-status {
    color: var(--text);
    font: var(--w-strong) var(--type-body) / 1.35 var(--font-sans);
  }
  .gauge-status.error {
    color: var(--err);
  }
  .gauge-status.preparation {
    color: var(--text-muted);
  }
</style>
