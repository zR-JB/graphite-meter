<script lang="ts">
  import { catalogSelection } from "../presentation/serverAppearance";
  import { onDestroy, untrack } from "svelte";
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
    JARGON,
    MISSING,
    OUTCOME,
    PHASE_HINT,
    STAGE,
    STATUS_TONE,
  } from "../presentation/vocabulary";
  import { announceChanges } from "../presentation/announcer.svelte";
  import { tooltipAction } from "../actions/tooltip";
  import { handoff } from "../presentation/motion.svelte";
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
  // Beside the latency card (landscape, from the 760 px container), the ring centres on the card's axis and its
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
  // Idle replies so far: what the stage's figures are drawn from, counted as they arrive.
  const replies = $derived(
    phase === "latency"
      ? store.latency.reduce(
          (count, bucket) =>
            bucket.phase === "latency"
              ? count + bucket.pingCount - bucket.timeoutCount
              : count,
          0,
        )
      : null,
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
      quietMs: store.live?.quietMs ?? null,
      unansweredMs,
      replies,
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
      };
    },
    ({ terminal, display }) =>
      terminal
        ? `${terminal.phase}:${terminal.value}`
        : `${display.value === MISSING}:${display.unit}`,
  );
  const { terminal, display } = $derived(hero.shown);
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
      // The reply count changes in place under one explainer, never fading per reply.
      const tip =
        known?.tip ?? (phase === "latency" ? JARGON.replies : undefined);
      return hint ? { hint: known?.text ?? hint, tip } : {};
    },
    // A note that counts keys on its explainer, so it updates in place.
    (notes: { status?: string; tone?: string; hint?: string; tip?: string }) =>
      `${notes.status}|${notes.tip ?? notes.hint}`,
  );
</script>

<section
  class="gauge-panel"
  data-phase={store.phase}
  data-stage={store.isRunning ? store.phaseStage : null}
  bind:clientWidth={panelWidth}
>
  <div class="instrument">
    <div class="dial panel" class:hung>
      <header class="panel-head dial-head">
        <span class="kicker">{msTicksActive ? "Latency" : "Throughput"}</span>
        {#if indicatedServers.length > 1}
          <div class="server-indicator">
            <ServerLens servers={indicatedServers} {participants} />
          </div>
        {/if}
      </header>
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
                <span
                  class="terminal-direction kicker"
                  data-tone={terminal.direction}
                >
                  <span class="swatch"></span>
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
              <span
                class="gauge-value"
                class:quiet={display.value === MISSING}
                aria-hidden="true">{display.value}</span
              >
              <span class="gauge-unit" aria-hidden="true">{display.unit}</span>
            {/if}
            <span class="sr-only">{spoken.value} {spoken.unit}</span>
          </div>
        </div>
      </div>
      <div
        class="gauge-footer"
        bind:clientHeight={noteHeight}
        style:top={hung
          ? `calc(100% - ${layout.height - layout.noteTop}px)`
          : null}
        style:opacity={footer.opacity}
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
      <div class="latency-slot"><LatencyProfile /></div>
    {/if}

    <div class="run-bar panel">
      <span class="kicker run-label">Stages</span>
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
    gap: var(--space-3);
    grid-template:
      "dial" minmax(260px, 40svh)
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
        / max(240px, (100% - 2 * var(--space-3)) / 3) minmax(0, 1fr);
    }
    .instrument:not(:has(.latency-slot)) {
      grid-template:
        "dial" minmax(220px, 1fr)
        "run" auto
        "results" auto
        / minmax(0, 1fr);
    }
  }
  /* Portrait, the dial is bound by its width, so it and the latency card share the width evenly. */
  @media (orientation: portrait) {
    @container viz (min-width: 760px) {
      .instrument:has(.latency-slot) {
        grid-template-columns: repeat(2, minmax(0, 1fr));
      }
    }
  }
  /* A phone keeps the dial compact, so the running stage's card shares the first screen with it; while the
     latency stage runs, its card is the one under the run button. */
  @container viz (max-width: 520px) {
    .instrument {
      grid-template-rows: clamp(240px, 34svh, 300px) repeat(3, auto);
      row-gap: var(--space-2);
    }
    .gauge-panel[data-stage="latency"] .instrument {
      grid-template-areas: "dial" "run" "latency" "results";
    }
  }
  /* The dial's frame: its head, the face, and the note under the ring; the face ends on the frame's edge, so a
     hung note measures from it. */
  .dial {
    grid-area: dial;
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .dial-head {
    flex: none;
    padding: var(--space-1) var(--space-2) 0 var(--space-3);
  }
  /* Framed like the dial and as tall; its content centres in the frame. A tight screen scrolls rather than
     overlapping the run bar. */
  .latency-slot {
    grid-area: latency;
    display: grid;
    min-width: 0;
  }
  .results {
    grid-area: results;
    min-width: 0;
  }
  /* The control strip: what runs next, and the one action that runs it, at the line's end. */
  .run-bar {
    grid-area: run;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2) var(--space-3);
    padding: var(--space-1) var(--space-1) var(--space-1) var(--space-3);
  }
  .run-bar > :global(.run-button) {
    margin-inline-start: auto;
  }
  /* A phone stacks them: the chips' row, then the run button across it. */
  @container viz (max-width: 520px) {
    .run-bar {
      display: grid;
      grid-template-columns: minmax(0, 1fr);
      gap: var(--space-2);
      padding: var(--space-2);
    }
    .run-bar > :global(.run-button) {
      margin-inline-start: 0;
    }
    .run-label {
      display: none;
    }
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
    font: 500 var(--type-2xs) / 1 var(--font-mono);
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
  /* The readout: mono, so a live value never shifts the layout, in the instrument's figure weight. */
  .gauge-value,
  .terminal-number {
    max-width: 100%;
    color: var(--text);
    font-family: var(--font-mono);
    font-weight: 500;
    font-variant-numeric: lining-nums tabular-nums;
    line-height: 1;
    letter-spacing: var(--track-figure);
    white-space: nowrap;
  }
  .gauge-value {
    min-width: 5ch;
    font-size: clamp(22px, 12.5cqmin, 56px);
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
  /* The stage the result names, engraved over it. */
  .terminal-direction {
    position: absolute;
    bottom: calc(100% + clamp(8px, 3.5cqmin, 14px));
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font-size: clamp(var(--type-2xs), 2.8cqmin, var(--type-xs));
  }
  @container (max-height: 180px) {
    .terminal-direction {
      display: none;
    }
  }
  .terminal-number {
    font-size: clamp(26px, 12.5cqmin, 56px);
  }
  /* Unit symbols are case-significant: Mbit/s, kB/s, MiB/s. One size and line height for both, so the result
     lands where the live value stood. */
  .terminal-unit,
  .gauge-unit {
    color: var(--text-muted);
    font: 500 clamp(var(--type-xs), 3.4cqmin, var(--type-md)) / var(--type-md)
      var(--font-mono);
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
