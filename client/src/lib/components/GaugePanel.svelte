<script lang="ts">
  import { onMount } from "svelte";
  import { prefersReducedMotion } from "svelte/motion";
  import { store } from "../state/store.svelte";
  import GaugeDial, { type GaugeDialState } from "./GaugeDial.svelte";
  import { GAUGE_LABEL_FRACTIONS, gaugeLayout } from "./gaugeLayout";
  import {
    fmtGaugeTick,
    throughputGaugeFraction,
    throughputValueAtFraction,
  } from "./gaugeScale";
  import StageTrack from "./StageTrack.svelte";
  import RunButton from "./RunButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import ResultCards from "./ResultCards.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { gaugeLatencyPresentation } from "./gaugeLatency";
  import {
    LiveRateAnimator,
    type LiveRateValues,
  } from "../presentation/liveRateAnimator";
  import {
    presentation,
    type PresentationHandle,
  } from "../canvas/presentation";
  import { primaryResultGaugeArc, resultGaugeArcs } from "./resultGauge";
  import { gaugeReadout } from "./gaugeReadout";
  import { ICON } from "../constants";

  const indicatedServers = $derived(
    store.serverDetails?.selection ??
      store.serverCatalog?.servers.filter((server) =>
        store.selectedServers.includes(server.id),
      ) ??
      [],
  );
  const serverIndicator = $derived(
    store.isRunning
      ? `Testing ${indicatedServers.length} servers`
      : store.result
        ? `Tested ${indicatedServers.length} servers`
        : `${indicatedServers.length} servers selected`,
  );
  const resultsView = $derived.by<"none" | "partial" | "final">(() => {
    if (store.phase === "complete") return "final";
    if (store.phase === "idle") return "none";
    return "partial";
  });
  const activeStagePresentation = $derived(
    store.phaseStage ? store.stagePresentation[store.phaseStage] : null,
  );
  const terminalArcs = $derived(resultGaugeArcs(store.result));
  const headlineArc = $derived(primaryResultGaugeArc(terminalArcs));
  // A one-sided bidirectional partial retains its lane result for diagnostics,
  // but has no truthful combined gauge value.
  const unusableStage = $derived(
    activeStagePresentation?.status === "failed" ||
      (store.phase === "complete" &&
        terminalArcs.length === 0 &&
        !store.result?.latency),
  );

  let stageEl = $state<HTMLDivElement>();
  let gaugeWidth = $state(0);
  let gaugeHeight = $state(0);
  const liveRateAnimator = new LiveRateAnimator();
  let liveRateValues = $state.raw<LiveRateValues>({
    transfer: 0,
    down: 0,
    up: 0,
  });
  let liveRatePresentation: PresentationHandle | null = null;

  const completedKind = $derived<"speed" | "latency">(
    terminalArcs.length ? "speed" : "latency",
  );
  const gaugeLatency = $derived.by(() => {
    return gaugeLatencyPresentation({
      phase: store.phase,
      liveRttMs: store.liveRtt,
      liveScaleMs: store.latencyScaleMs,
      history: store.latency,
      completedRttMs:
        store.phase === "complete" && terminalArcs.length === 0
          ? (store.result?.latency?.reportedMs ?? null)
          : null,
    });
  });

  const msTicksActive = $derived(
    store.phase === "latency" ||
      (store.phase === "complete" && completedKind === "latency"),
  );
  const gaugeScaleBytesPerSec = $derived(store.gaugeScaleBytesPerSec);
  const gaugeUnit = $derived(store.unitLabel);
  const gaugeRate = (bytesPerSec: number) => store.toUnit(bytesPerSec);
  const gaugeTicks = $derived.by(() => {
    if (msTicksActive)
      return GAUGE_LABEL_FRACTIONS.map((fraction) => ({
        fraction,
        label: fmtMs(gaugeLatency.scaleMs * fraction),
      }));
    return GAUGE_LABEL_FRACTIONS.map((fraction) => ({
      fraction,
      label: fmtGaugeTick(
        gaugeRate(throughputValueAtFraction(fraction, gaugeScaleBytesPerSec)),
      ),
    }));
  });
  const layout = $derived(gaugeLayout(gaugeWidth, gaugeHeight));
  const throughputEvidence = $derived(
    (store.phase === "download" ||
      store.phase === "upload" ||
      store.phase === "bidirectional") &&
      store.liveThroughput.some((sample) => sample.phase === store.phase),
  );
  const showGaugeTicks = $derived(
    !unusableStage &&
      (store.phase === "latency" ||
        store.phase === "download" ||
        store.phase === "upload" ||
        store.phase === "bidirectional" ||
        store.phase === "complete") &&
      gaugeTicks.length > 1,
  );

  const liveRateInput = $derived.by(() => {
    const phase = store.phase;
    const bidi = store.visualBidirectional ?? { down: 0, up: 0 };
    return {
      active:
        store.measuring &&
        (phase === "download" ||
          phase === "upload" ||
          phase === "bidirectional"),
      context: `${store.runSeq}:${phase}`,
      values: {
        transfer: store.visualTransferBytesPerSec,
        down: bidi.down,
        up: bidi.up,
      },
    };
  });

  function stepLiveRates(now: number): boolean {
    const frame = liveRateAnimator.step(
      liveRateInput,
      now,
      prefersReducedMotion.current,
    );
    liveRateValues = frame.values;
    return frame.active;
  }

  $effect(() => {
    void liveRateInput;
    void prefersReducedMotion.current;
    liveRatePresentation?.invalidate();
  });

  const readout = $derived(
    gaugeReadout({
      phase: store.phase,
      running: store.isRunning,
      preparing: store.preparing,
      preparation: store.preparation,
      startError: store.startError,
      error: store.error,
      aggregateEvidence: store.aggregateEvidence,
      latencyTimeout: store.liveLatencyLost,
      latencyMs: gaugeLatency.rttMs,
      hasLatencyResult: !!store.result?.latency,
      unusable: unusableStage,
      arcs: terminalArcs,
      headline: headlineArc,
      animatedBytesPerSec: liveRateValues.transfer,
      measuredBytesPerSec: store.liveTransferBytesPerSec,
      rate: (bytesPerSec) => fmtSpeed(gaugeRate(bytesPerSec)),
      unit: gaugeUnit,
    }),
  );

  // The live region mirrors a per-frame value. Mid-phase announcements wait a
  // second apart, the time a screen reader needs to finish a sentence. Phase
  // changes and idle updates jump the queue.
  const ANNOUNCE_INTERVAL_MS = 1000;
  let announcement = $state("");
  let pendingAnnouncement = "";
  let announceTimer: ReturnType<typeof setTimeout> | null = null;
  let lastAnnouncedAt = -Infinity;
  let lastAnnouncedPhase = "";
  $effect(() => {
    const phase = store.phase;
    pendingAnnouncement = readout.announcement;
    const commit = () => {
      announcement = pendingAnnouncement;
      lastAnnouncedAt = performance.now();
      lastAnnouncedPhase = phase;
      announceTimer = null;
    };
    if (!store.isRunning || phase !== lastAnnouncedPhase) {
      if (announceTimer) clearTimeout(announceTimer);
      commit();
    } else if (!announceTimer) {
      announceTimer = setTimeout(
        commit,
        Math.max(
          0,
          ANNOUNCE_INTERVAL_MS - (performance.now() - lastAnnouncedAt),
        ),
      );
    }
  });

  const dialState = $derived.by<GaugeDialState>(() => {
    const p = store.phase;
    const scale = store.gaugeScaleBytesPerSec;
    return {
      phase: p,
      showValue: !unusableStage,
      valueBytesPerSec: unusableStage
        ? 0
        : p === "complete" && headlineArc
          ? headlineArc.bytesPerSec
          : store.visualTransferBytesPerSec,
      scaleBytesPerSec: scale,
      throughputEvidence:
        p === "complete" ? terminalArcs.length > 0 : throughputEvidence,
      latencyScaleMs: gaugeLatency.scaleMs,
      rtt: gaugeLatency.rttMs,
      completedKind,
      resultArcs:
        p === "complete"
          ? terminalArcs.map((arc) => ({
              phase: arc.phase,
              fraction: throughputGaugeFraction(arc.bytesPerSec, scale),
              dashed: arc.dashed,
              description: [
                `${arc.label}${arc.dashed ? " — partial" : ""}`,
                `${fmtSpeed(gaugeRate(arc.bytesPerSec))} ${gaugeUnit}`,
                ...(arc.phase === "bidirectional"
                  ? (["down", "up"] as const).flatMap((direction) => {
                      const lane = store.result?.bidirectional?.[direction];
                      return lane
                        ? [
                            `${direction === "down" ? "Download" : "Upload"}: ${fmtSpeed(gaugeRate(lane.reportedBytesPerSec))} ${gaugeUnit}`,
                          ]
                        : [];
                    })
                  : []),
              ].join("\n"),
            }))
          : [],
    };
  });

  onMount(() => {
    liveRatePresentation = presentation.register(stageEl!, stepLiveRates);
    return () => {
      if (announceTimer) clearTimeout(announceTimer);
      liveRatePresentation?.destroy();
      liveRatePresentation = null;
    };
  });
</script>

<section class="gauge-panel" data-phase={store.phase}>
  <!-- One container-query grid switches the complete instrument layout and
       keeps the gauge track stable when the latency panel is toggled. -->
  <div class="instrument">
    <div class="well stage">
      {#if indicatedServers.length > 1}
        <div
          class="server-indicator"
          title={indicatedServers.map((server) => server.name).join(", ")}
        >
          <svg viewBox="0 0 20 20" aria-hidden="true"
            ><rect x="2.5" y="3" width="15" height="5" rx="1.5" /><rect
              x="2.5"
              y="12"
              width="15"
              height="5"
              rx="1.5"
            /><path d="M6 5.5h.01M6 14.5h.01M10 8v4" /></svg
          >
          <span>{serverIndicator}</span>
        </div>
      {/if}
      <div
        bind:this={stageEl}
        bind:clientWidth={gaugeWidth}
        bind:clientHeight={gaugeHeight}
        class="gauge-face"
        style:--gauge-center-offset={`${layout.center.y - layout.height / 2}px`}
      >
        <GaugeDial input={dialState} {layout} />
        {#if showGaugeTicks}
          <div class="gauge-ticks" aria-hidden="true">
            {#each layout.labelPoints as point, index (index)}
              <span
                class="gauge-tick"
                data-anchor-x={point.anchorX}
                data-anchor-y={point.anchorY}
                style:left={`${point.x}px`}
                style:top={`${point.y}px`}>{gaugeTicks[index].label}</span
              >
            {/each}
          </div>
        {/if}
        <div class="metric-wrap">
          {#if readout.terminal}
            <div
              class="terminal-readout"
              class:partial={readout.terminal.dashed}
              aria-hidden="true"
            >
              <span class="terminal-direction">
                <span
                  class="tone-icon terminal-icon"
                  data-tone={readout.terminal.direction}
                >
                  {#if readout.terminal.direction === "download"}
                    {@html ICON.download}
                  {:else if readout.terminal.direction === "upload"}
                    {@html ICON.upload}
                  {:else}
                    {@html ICON.bidirectional}
                  {/if}
                </span>
                {readout.terminal.direction === "download"
                  ? "Download"
                  : readout.terminal.direction === "upload"
                    ? "Upload"
                    : "Bidirectional"}
              </span>
              <span class="terminal-number">{readout.terminal.value}</span>
              <span class="terminal-unit">{gaugeUnit}</span>
              {#if readout.terminal.dashed}
                <span class="terminal-partial"
                  >Partial {readout.terminal.direction}</span
                >
              {/if}
            </div>
          {:else}
            {#if readout.display.value}<span
                class="gauge-value"
                aria-hidden="true">{readout.display.value}</span
              >{/if}
            {#if readout.display.unit}<span
                class="gauge-unit"
                aria-hidden="true">{readout.display.unit}</span
              >{/if}
          {/if}
          <span class="sr-only"
            >{readout.announced.value} {readout.announced.unit}</span
          >
        </div>
      </div>
      <div class="gauge-footer">
        {#if readout.hint || readout.status || readout.failure}
          <div class="gauge-notes">
            {#if store.preparing}
              <span class="gauge-status preparation"
                >{readout.preparationLabel}</span
              >
            {:else if readout.failure}
              <span class="gauge-status error">{readout.failure.headline}</span>
              <span class="gauge-hint">{readout.failure.detail}</span>
            {:else if readout.status}
              <span class="gauge-status" class:error={readout.status.error}>
                {readout.status.headline}
              </span>
              <span class="gauge-hint">{readout.status.action}</span>
            {:else if readout.hint}<span class="gauge-hint">{readout.hint}</span
              >{/if}
          </div>
        {/if}
      </div>
      <output class="sr-only" aria-live="polite">{announcement}</output>
    </div>

    <div class="instrument-controls">
      <div class="run-slot"><RunButton /></div>
      <div class="stage-head"><StageTrack /></div>
    </div>

    {#if store.latencyEnabled}
      <div class="well latency-panel">
        <LatencyProfile />
      </div>
    {/if}
  </div>

  <div class="results-slot">
    {#if resultsView === "partial"}
      <ResultCards compact liveRates={liveRateValues} />
    {:else if resultsView === "final"}
      <ResultCards />
    {/if}
  </div>
</section>

<style>
  /* Faceplate: the panel is flat and transparent. The gauge and latency
     panels are wells milled into it; the controls sit on the faceplate. */
  .gauge-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    /* Query context for .instrument, so a docked panel shrinking this column
       restyles it. It sits here: a container query only styles descendants. */
    container: viz / inline-size;
  }
  /* The instrument owns the gauge, profile and controls as one responsive grid. */
  .instrument {
    /* One readable gauge size across live and completed states. The profile
       contributes its intrinsic height instead of a nested scroller. */
    --gauge-well-height: clamp(280px, 35svh, 360px);
    display: grid;
    gap: var(--space-3) var(--space-2);
    grid-template:
      "gauge" var(--gauge-well-height)
      "controls" auto
      "latency" auto
      / 1fr;
  }
  .instrument:not(:has(.latency-panel)) {
    grid-template:
      "gauge" var(--gauge-well-height)
      "controls" auto
      / 1fr;
  }
  /* Wide instruments pair the two readings above their controls. */
  @container viz (min-width: 760px) {
    .instrument {
      grid-template:
        "gauge latency" minmax(var(--gauge-well-height), auto)
        "controls controls" auto
        / minmax(240px, 1fr) minmax(240px, 1fr);
    }
    /* The gauge keeps its size when latency is disabled. */
    .instrument:not(:has(.latency-panel)) {
      grid-template:
        "gauge gauge" var(--gauge-well-height)
        "controls controls" auto
        / minmax(240px, 1fr) minmax(240px, 1fr);
    }
  }
  @media (min-width: 1800px) and (min-height: 1000px) {
    .instrument {
      --gauge-well-height: clamp(360px, min(43svh, 32cqw), 560px);
    }
  }
  @media (max-width: 759px) and (orientation: portrait) {
    .instrument {
      /* A phone retains a readable dial while the document carries results. */
      --gauge-well-height: clamp(280px, 32svh, 320px);
    }
    .stage {
      min-height: 280px;
    }
  }
  /* The gauge well: the deepest recess on the faceplate. */
  .stage {
    grid-area: gauge;
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 240px;
    min-height: 220px;
    overflow: hidden;
  }
  .latency-panel {
    grid-area: latency;
    display: flex;
    flex-direction: column;
    justify-content: center;
    min-width: 240px;
    min-height: 220px;
    padding: var(--space-2);
  }
  .server-indicator {
    position: absolute;
    z-index: 1;
    inset: var(--space-3) auto auto var(--space-3);
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-xs) / 1.4 var(--font-sans);
  }
  .server-indicator svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linecap: round;
  }
  .gauge-face {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
    /* Size container so the hero number scales with cqmin, the dimension
       that sizes the ring. cqw overflows a wide, short well. */
    container-type: size;
  }
  .instrument-controls {
    --stage-controls-width: 540px;
    grid-area: controls;
    display: grid;
    align-items: center;
    justify-self: center;
    gap: var(--space-3);
    width: 100%;
    padding-block: var(--space-2);
  }
  .instrument-controls:has(:global(.quad)) {
    --stage-controls-width: 700px;
  }
  /* Short wide windows share a control row; taller screens keep the action
     above its stages. */
  @media (max-height: 800px) {
    .instrument-controls {
      padding-block: var(--space-1);
    }
    @container viz (min-width: 1000px) {
      .instrument-controls {
        grid-template-columns: 280px minmax(0, 1fr);
        align-items: end;
        column-gap: var(--space-5);
        max-width: calc(280px + var(--space-5) + var(--stage-controls-width));
      }
    }
  }
  .run-slot {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 46px;
  }
  .stage-head {
    width: 100%;
    max-width: var(--stage-controls-width);
    justify-self: center;
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
    font: 600 var(--type-2xs) / 1 var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    opacity: 0.75;
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
    /* Keeps the number clear of the ring's sides; the inline padding also
       bounds how wide the value grows until cqmin sizing reins it in. */
    padding-inline: 9%;
    padding-top: calc(2 * var(--gauge-center-offset));
  }
  /* The hero number: tabular figures so a live value never shifts layout,
     sized in cqmin so large numbers shrink to fit a narrow gauge. */
  .gauge-value,
  .terminal-number {
    max-width: 100%;
    color: var(--text);
    font-family: var(--font-display);
    font-weight: 600;
    font-variant-numeric: lining-nums tabular-nums;
    letter-spacing: var(--track-tight);
    white-space: nowrap;
  }
  .gauge-value {
    font-size: clamp(20px, 14cqmin, 64px);
    line-height: 0.95;
  }
  .terminal-readout {
    position: relative;
    display: grid;
    justify-items: center;
    gap: var(--space-1);
    max-width: 72%;
  }
  .terminal-direction {
    position: absolute;
    bottom: calc(100% + clamp(10px, 4cqmin, 16px));
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font-size: clamp(var(--type-xs), 3.6cqmin, 13px);
    font-weight: 600;
    line-height: 1;
    white-space: nowrap;
  }
  .terminal-icon {
    width: 20px;
    height: 20px;
  }
  .terminal-number {
    font-size: clamp(28px, 15.5cqmin, 62px);
    line-height: 1;
  }
  /* Unit symbols are case-significant: Mbit/s, kB/s, MiB/s. */
  .terminal-unit,
  .gauge-unit {
    color: var(--text-soft);
    font-family: var(--font-mono);
    line-height: 1;
  }
  .terminal-unit {
    font-size: clamp(var(--type-xs), 3.8cqmin, var(--type-md));
    font-weight: var(--w-normal);
  }
  .gauge-unit {
    margin-top: var(--space-1);
    font-size: var(--type-sm);
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .terminal-partial {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  /* A separate footer keeps transient notes from overlapping the dial. */
  .gauge-footer {
    display: grid;
    align-items: center;
    min-height: 44px;
    padding: var(--space-2) var(--space-3) var(--space-3);
  }
  .gauge-notes {
    display: grid;
    gap: var(--space-1);
    text-align: center;
  }
  .gauge-hint {
    color: var(--text-muted);
    font-size: var(--type-sm);
    font-weight: 600;
    line-height: 1.35;
  }
  /* Aborted / error headline above the softer action line. A user abort
     stays neutral at full text strength so the state is unmissable. */
  .gauge-status {
    color: var(--text);
    font: 700 var(--type-xs) var(--font-mono);
    letter-spacing: var(--track-wide);
    text-transform: uppercase;
  }
  .gauge-status.error {
    color: var(--err);
  }
  .gauge-status.preparation {
    color: var(--brand-strong);
  }
  .results-slot:empty {
    display: none;
  }
</style>
