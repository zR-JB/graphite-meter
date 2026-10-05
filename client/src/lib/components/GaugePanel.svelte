<script lang="ts">
  import { catalogSelection } from "../presentation/serverAppearance";
  import Icon from "./Icon.svelte";
  import { onDestroy, untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import GaugeDial, {
    RESULT_DRAIN_MS,
    type GaugeDialState,
  } from "./GaugeDial.svelte";
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
    JARGON,
    MISSING,
    OUTCOME,
    PHASE_HINT,
    STAGE,
    STATUS_TONE,
  } from "../presentation/vocabulary";
  import { announceChanges } from "../presentation/announcer.svelte";
  import { tooltipAction } from "../actions/tooltip";
  import { HANDOFF_OUT_MS, handoff } from "../presentation/motion.svelte";
  import { MediaQuery } from "svelte/reactivity";

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
  let panelWidth = $state(0);
  let noteHeight = $state(0);
  // Beside the latency card (landscape, from a 760 px panel), the ring centres on the card's axis and its
  // note hangs just under it; portrait and phones keep the note in the column's flow.
  const portrait = new MediaQuery("(orientation: portrait)");
  const hung = $derived(panelWidth >= 760 && !portrait.current);
  const liveReadout = new LiveReadout();
  onDestroy(() => liveReadout.dispose());
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

  // The latency stage's own warmup already reads in milliseconds, so the scale changes kind once.
  const msTicksActive = $derived(
    phase === "latency" ||
      (phase === "warmup" && store.phaseStage === "latency") ||
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
  const layout = $derived(
    gaugeLayout(gaugeWidth, gaugeHeight, hung ? noteHeight : 0),
  );
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

  // While the latency stage's probes go unanswered, for how long: from the last reply to the newest bucket.
  const unansweredMs = $derived.by(() => {
    if (phase !== "latency" || !store.liveLatencyLost) return null;
    const newest = store.latency.at(-1)?.endT ?? 0;
    const answered =
      store.latency.findLast(
        (bucket) => bucket.phase === "latency" && bucket.medianRttMs !== null,
      )?.endT ?? store.phaseStartedAtMs;
    return newest - answered >= 1000 ? newest - answered : null;
  });
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
      quietMs: store.live?.quietMs ?? null,
      unansweredMs,
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
  const arcs = $derived(
    phase === "complete"
      ? terminalArcs.map((arc) => ({
          phase: arc.phase,
          fraction: throughputGaugeFraction(
            arc.bytesPerSec,
            store.scales.gaugeBytesPerSec,
          ),
          dashed: arc.dashed,
          primary: arc === headlineArc,
          description: `${arc.label}${arc.dashed ? ` · ${OUTCOME.partial}` : ""}\n${fmtSpeed(gaugeRate(arc.bytesPerSec))} ${gaugeUnit}`,
        }))
      : [],
  );
  const hero = handoff(
    () => {
      return {
        terminal: readout.terminal,
        display: rateDisplay(liveRates),
        unit: gaugeUnit,
        arcs,
        live: store.isRunning ? store.phaseStage : null,
      };
    },
    // A new stage hands the figure and its name off together.
    ({ terminal, display, live }) =>
      terminal
        ? `${terminal.phase}:${terminal.value}`
        : `${live}:${display.value === MISSING}:${display.unit}`,
    // A result leaves as its arcs drain back to zero (GaugeDial), so the next run starts from an empty ring.
    (leaving) => (leaving.arcs.length ? RESULT_DRAIN_MS : HANDOFF_OUT_MS),
  );
  const { terminal, display, live } = $derived(hero.shown);
  // The dial beats only on idle replies, so a stall shows as stillness.
  const reply = $derived(
    phase === "latency"
      ? (store.latency.findLast((bucket) => bucket.medianRttMs !== null)?.t ??
          null)
      : null,
  );
  const dialState = $derived<GaugeDialState>({
    phase,
    reply,
    showValue: !unusableStage,
    stage: store.isRunning ? store.phaseStage : null,
    valueBytesPerSec: liveRates ? liveRates.down + liveRates.up : 0,
    scaleBytesPerSec: store.scales.gaugeBytesPerSec,
    throughputEvidence: liveRates !== null,
    latencyScaleMs: gaugeLatency.scaleMs,
    rtt: liveReadout.rtt.current,
    completedKind,
  });
  const footer = handoff(
    () => {
      const { hint, status, failure, noData, noReplies } = readout;
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
      if (noData) return { hint: noData, tip: JARGON.noData };
      if (noReplies) return { hint: noReplies, tip: JARGON.noReplies };
      const known = PHASE_HINT[phase];
      return hint ? { hint: known?.text ?? hint, tip: known?.tip } : {};
    },
    // A note that counts keys on its explainer, so it updates in place.
    (notes: { status?: string; tone?: string; hint?: string; tip?: string }) =>
      `${notes.status}|${notes.tip ?? notes.hint}`,
  );
</script>

<section
  class="gauge-panel"
  class:wide={panelWidth >= 760}
  class:compact={panelWidth <= 520}
  class:tight={panelWidth <= 430}
  style:--panel-width="{panelWidth}px"
  data-phase={store.phase}
  data-stage={store.isRunning ? store.phaseStage : null}
  bind:clientWidth={panelWidth}
>
  <div class="instrument">
    <div class="dial" class:hung data-flip="dial">
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
        style:--face-min="{Math.min(gaugeWidth, gaugeHeight)}px"
      >
        <GaugeDial
          input={dialState}
          {layout}
          result={{ arcs: hero.shown.arcs, out: hero.out }}
        />
        {#if ticks.shown.length > 1}
          <div
            class="gauge-ticks handoff"
            class:handoff-out={ticks.out}
            aria-hidden="true"
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
        <div class="metric-wrap handoff" class:handoff-out={hero.out}>
          <div
            class="hero"
            class:terminal={!!terminal}
            class:short={gaugeHeight < 180}
          >
            {#if terminal}
              <div
                class="terminal-readout"
                class:partial={terminal.dashed}
                aria-hidden="true"
              >
                <span class="terminal-direction" data-tone={terminal.direction}>
                  <span class="tone-icon" aria-hidden="true"
                    ><Icon name={STAGE[terminal.direction].icon} /></span
                  >
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
              <div class="terminal-readout" aria-hidden="true">
                <!-- The running stage names itself over its figure, as the result does. -->
                {#if live}
                  <span class="terminal-direction" data-tone={live}>
                    <span class="tone-icon" aria-hidden="true"
                      ><Icon name={STAGE[live].icon} /></span
                    >
                    {STAGE[live].label}
                  </span>
                {/if}
                <span
                  class="gauge-value"
                  class:quiet={display.value === MISSING}>{display.value}</span
                >
                <span class="gauge-unit">{display.unit}</span>
              </div>
            {/if}
            <span class="sr-only">{spoken.value} {spoken.unit}</span>
          </div>
        </div>
      </div>
      <div
        class="gauge-footer handoff"
        bind:clientHeight={noteHeight}
        class:handoff-out={footer.out}
        style:top={hung
          ? `calc(100% - ${layout.height - layout.noteTop}px)`
          : null}
      >
        {#if footer.shown.status || footer.shown.hint}
          {@const { status, tone, hint, tip } = footer.shown}
          <div class="gauge-notes">
            {#if status}
              <span class="gauge-status {tone}">{status}</span>
            {/if}
            {#if hint}
              <span class="gauge-hint" use:tooltipAction={tip ?? ""}
                >{hint}</span
              >
            {/if}
          </div>
        {/if}
      </div>
    </div>

    {#if store.latencyEnabled}
      <div class="latency-panel" data-flip="lanes"><LatencyProfile /></div>
    {/if}

    <!-- The run key over the stage chips, under the lanes beside the dial. -->
    <div class="controls" data-flip="controls">
      <StageTrack><RunButton /></StageTrack>
    </div>

    <div class="results"><ResultCards live={liveReadout} /></div>
  </div>
</section>

<style>
  /* The panel's width classes come from its measured box: as a size container it would restyle the whole
     instrument on every frame that changes a figure. */
  .gauge-panel {
    height: 100%;
  }
  /* The dial's panel beside the latency panel, with the run key and the stage chips under the lanes; the dial
     spans both rows. Each row is as tall as its content, so the console ends where the cards end and the spare
     height stays on the canvas below; what little the dial's ring adds over the lanes goes around the controls.
     Without the latency stage the dial stands centred with the controls under it. */
  .instrument {
    --panel-width: inherit;
    --dial-height: clamp(320px, 40svh, 380px);
    display: grid;
    gap: var(--space-4);
    grid-template:
      "dial" var(--dial-height)
      "controls" auto
      "results" auto
      "latency" auto
      / minmax(0, 1fr);
  }
  .instrument:not(:has(.latency-panel)) {
    grid-template:
      "dial" var(--dial-height)
      "controls" auto
      "results" auto
      / minmax(0, 1fr);
  }
  .controls {
    grid-area: controls;
    display: grid;
    align-content: center;
    justify-items: center;
    min-width: 0;
  }
  /* The dial's figure rises into place as each stage's value arrives. */
  .metric-wrap {
    --rise: 6px;
  }
  /* A tall screen's spare height goes into even air above, between and below the sections, not under the cards. */
  .wide .instrument {
    --dial-width: clamp(300px, var(--panel-width) * 0.3, 560px);
    --dial-ratio: 0.86;
    min-height: 100%;
    align-content: space-evenly;
    grid-template:
      "dial latency" auto
      "dial controls" auto
      "results results" auto
      / var(--dial-width) minmax(0, 1fr);
  }
  /* The dial is as tall as its ring wants, or as the lanes and the controls together, whichever is more. */
  .wide .instrument .dial {
    min-height: calc(var(--dial-width) * var(--dial-ratio));
  }
  /* The lanes start a step under the dial's top, nearer the ring's crown than its box. */
  .wide .latency-panel {
    margin-top: var(--space-6);
  }
  .wide .instrument:not(:has(.latency-panel)) {
    --dial-width: clamp(360px, var(--panel-width) * 0.48, 720px);
    --dial-ratio: 0.6;
    grid-template:
      "dial" auto
      "controls" auto
      "results" auto
      / minmax(0, 1fr);
  }
  .wide .instrument:not(:has(.latency-panel)) .dial {
    justify-self: center;
    width: var(--dial-width);
  }
  .compact .instrument {
    gap: var(--space-3);
  }
  /* The dial's panel: the face, and the note under the ring; the face ends on the note, so a hung note measures
     from it. */
  .dial {
    --dial-width: inherit;
    grid-area: dial;
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  .latency-panel {
    grid-area: latency;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .results {
    grid-area: results;
    display: grid;
    min-width: 0;
    min-height: 0;
  }
  .server-indicator {
    position: absolute;
    z-index: 1;
    inset: var(--space-2) auto auto var(--space-2);
    display: flex;
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
    /* The hero's type scales with --face-min, the measured dimension that sizes the ring: measured, not a
       container query, which a flex item answers late. */
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
    font: var(--w-normal) var(--type-xs) / 1 var(--font-sans);
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
  /* Tabular figures keep a live value from shifting layout; the readout is the page's largest figure. */
  .gauge-value,
  .terminal-number {
    max-width: 100%;
    color: var(--text);
    font-family: var(--font-mono);
    font-weight: 500;
    font-variant-numeric: lining-nums tabular-nums;
    line-height: 1;
    white-space: nowrap;
  }
  .gauge-value {
    min-width: 5ch;
    font-size: clamp(24px, calc(var(--face-min) * 0.17), 76px);
    text-align: center;
  }
  /* "—" waits quietly where the value arrives, like the cards'. */
  .gauge-value.quiet {
    color: var(--text-soft);
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
  /* The stage the result names, over the number, out of the readout's flow so the result lands where the live
     value stood. */
  .terminal-direction {
    position: absolute;
    bottom: calc(100% + clamp(10px, var(--face-min) * 0.04, 16px));
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font: var(--w-strong) clamp(var(--type-xs), var(--face-min) * 0.036, 14px) /
      1 var(--font-sans);
    white-space: nowrap;
  }
  .terminal-direction .tone-icon {
    width: 18px;
    height: 18px;
  }
  .terminal-direction .tone-icon :global(svg) {
    width: 10px;
    height: 10px;
  }
  /* Decided from the measured face, not a container query: a size-contained flex item answers one late. */
  .hero.short .terminal-direction {
    display: none;
  }
  .terminal-number {
    font-size: clamp(30px, calc(var(--face-min) * 0.17), 76px);
  }
  /* Unit symbols are case-significant: Mbit/s, kB/s, MiB/s. One size and line height for both, so the result
     lands where the live value stood. */
  .terminal-unit,
  .gauge-unit {
    color: var(--text-muted);
    font: var(--w-normal)
      clamp(var(--type-sm), var(--face-min) * 0.04, var(--type-lg)) /
      var(--type-md) var(--font-sans);
  }
  /* Empty, it keeps its line, so "—" sits where the value arrives. */
  .gauge-unit {
    min-height: 1lh;
    margin-top: var(--space-1);
  }
  /* Under the readout, out of its flow, so the value stays where it landed. */
  .terminal-partial {
    position: absolute;
    top: calc(100% + var(--space-1));
    color: var(--tone-ink);
    font-size: var(--type-xs);
  }
  /* A separate footer keeps notes off the dial; it holds two lines, so a longer note never shrinks the ring. */
  .gauge-footer {
    display: grid;
    align-items: center;
    min-height: calc(var(--space-2) + 2.7 * var(--type-body));
    padding: var(--space-1) var(--space-3) var(--space-2);
  }
  /* Hung under the ring's tick ends, out of the column's flow, in a band gaugeLayout keeps free; one line sits up top. */
  .hung .gauge-footer {
    position: absolute;
    inset-inline: 0;
    align-items: start;
    height: calc(var(--space-2) + 2.7 * var(--type-body));
  }
  .gauge-notes {
    display: grid;
    gap: 2px;
    text-align: center;
  }
  .gauge-hint {
    color: var(--text-muted);
    font: var(--w-normal) var(--type-sm) / 1.35 var(--font-sans);
  }
  .gauge-status {
    color: var(--text);
    font: var(--w-strong) var(--type-sm) / 1.35 var(--font-sans);
  }
  .gauge-status.error {
    color: var(--err);
  }
  .gauge-status.preparation {
    color: var(--text-muted);
  }
</style>
