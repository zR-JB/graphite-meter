<script lang="ts">
  import { onMount } from "svelte";
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
  import { fmtSpeed, fmtMs, reasonLabel } from "../format";
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
  import { preparationFailurePresentation } from "./preparationFailure";
  import { failureDetail } from "./failurePresentation";
  import { ICON } from "../constants";

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
  let gaugeSize = $state({ width: 0, height: 0 });
  const liveRateAnimator = new LiveRateAnimator();
  let liveRateValues = $state<LiveRateValues>({
    transfer: 0,
    down: 0,
    up: 0,
  });
  let liveRatePresentation: PresentationHandle | null = null;
  let reducedRateMotion = false;

  const EMPTY_DISPLAY = { value: "—", unit: "" };
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
  const layout = $derived(gaugeLayout(gaugeSize.width, gaugeSize.height));
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

  const display = $derived.by(() => {
    const p = store.phase;
    if (unusableStage) return EMPTY_DISPLAY;
    if (p === "latency")
      return store.liveLatencyLost
        ? { value: "—", unit: "probe timeout" }
        : { value: fmtMs(gaugeLatency.rttMs), unit: "ms" };
    if (
      p === "idle" ||
      p === "connecting" ||
      p === "error" ||
      p === "aborted" ||
      p === "warmup"
    )
      return EMPTY_DISPLAY;
    if (p === "complete") {
      if (headlineArc)
        return {
          value: fmtSpeed(gaugeRate(headlineArc.bytesPerSec)),
          unit: `${gaugeUnit} · ${headlineArc.label}`,
        };
      return store.result?.latency
        ? { value: fmtMs(gaugeLatency.rttMs), unit: "ms" }
        : EMPTY_DISPLAY;
    }
    return {
      value: fmtSpeed(gaugeRate(liveRateValues.transfer)),
      unit: gaugeUnit,
    };
  });

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
    const frame = liveRateAnimator.step(liveRateInput, now, reducedRateMotion);
    liveRateValues = frame.values;
    return frame.active;
  }

  $effect(() => {
    void liveRateInput;
    liveRatePresentation?.invalidate();
  });

  // The visible display may follow the bounded upload bridge, but a live
  // announcement describes measured throughput and must remain authoritative.
  const announcementDisplay = $derived.by(() => {
    if (
      store.phase === "download" ||
      store.phase === "upload" ||
      store.phase === "bidirectional"
    )
      return {
        value: fmtSpeed(gaugeRate(store.liveTransferBytesPerSec)),
        unit: gaugeUnit,
      };
    return display;
  });

  const terminalAnnouncement = $derived(
    terminalArcs
      .map(
        (arc) =>
          `${arc.label} ${fmtSpeed(gaugeRate(arc.bytesPerSec))} ${gaugeUnit}${arc.dashed ? ", partial" : ""}`,
      )
      .join("; "),
  );
  const terminalPrimary = $derived.by(() => {
    const arc = store.phase === "complete" ? headlineArc : null;
    return arc
      ? {
          ...arc,
          value: fmtSpeed(gaugeRate(arc.bytesPerSec)),
          direction:
            arc.phase === "download" || arc.label.endsWith("download")
              ? ("download" as const)
              : arc.phase === "upload" || arc.label.endsWith("upload")
                ? ("upload" as const)
                : ("bidirectional" as const),
        }
      : null;
  });

  const STAGE_NAME: Record<string, string> = {
    latency: "Latency",
    download: "Download",
    upload: "Upload",
    bidirectional: "Bidirectional",
  };
  const failNotes = $derived(
    store.transferFailures.map(
      (f) => `${STAGE_NAME[f.stage]} skipped — ${failureDetail(f.message)}`,
    ),
  );

  const hint = $derived.by(() => {
    if (store.preparing) return "Checking paths";
    switch (store.phase) {
      case "idle":
        return "Press Start test to start your speed test";
      case "connecting":
        return "Verifying the selected protocol…";
      case "warmup":
        return "Checking your connection…";
      default:
        return "";
    }
  });

  const preparationPathLabel = (state: string): string => {
    if (state === "disabled") return "not needed";
    return state;
  };
  const preparationAnnouncement = $derived.by(() => {
    if (!store.preparing) return "";
    return `Starting test. Throughput path ${preparationPathLabel(store.preparation.throughput)}; Latency path ${preparationPathLabel(store.preparation.latency)}`;
  });
  const preparationFailure = $derived(
    preparationFailurePresentation(store.preparation, store.startError),
  );

  const status = $derived.by(() => {
    switch (store.phase) {
      case "aborted":
        return {
          tone: "aborted",
          headline: "Test aborted",
          action: "Press Run Again to restart",
        };
      case "error":
        return {
          tone: "error",
          headline: store.error
            ? reasonLabel(store.error.reason)
            : "Something went wrong",
          action: "Press Run Again to retry",
        };
      default:
        return null;
    }
  });

  const statusText = $derived(
    store.preparing
      ? preparationAnnouncement
      : preparationFailure
        ? `${preparationFailure.headline} — ${preparationFailure.detail}`
        : status
          ? `${status.headline} — ${status.action}`
          : hint,
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
    pendingAnnouncement =
      statusText ||
      (phase === "complete" && terminalAnnouncement) ||
      `${announcementDisplay.value} ${announcementDisplay.unit}, phase ${phase}`;
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
    const rateMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedRateMotion = rateMotion.matches;
    const onRateMotion = (event: MediaQueryListEvent) => {
      reducedRateMotion = event.matches;
      liveRatePresentation?.invalidate();
    };
    rateMotion.addEventListener("change", onRateMotion);
    const resizeObserver = new ResizeObserver((entries) => {
      const { width, height } = entries[0].contentRect;
      if (gaugeSize.width !== width || gaugeSize.height !== height)
        gaugeSize = { width, height };
    });
    resizeObserver.observe(stageEl!);
    const { clientWidth: width, clientHeight: height } = stageEl!;
    gaugeSize = { width, height };

    return () => {
      if (announceTimer) clearTimeout(announceTimer);
      rateMotion.removeEventListener("change", onRateMotion);
      liveRatePresentation?.destroy();
      liveRatePresentation = null;
      resizeObserver.disconnect();
    };
  });
</script>

<section class="gauge-panel">
  <!-- One container-query grid switches the complete instrument layout and
       keeps the gauge track stable when the latency panel is toggled. -->
  <div class="instrument">
    <div class="stage">
      <div
        bind:this={stageEl}
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
          {#if terminalPrimary}
            <div
              class="terminal-readout {terminalPrimary.phase}"
              class:partial={terminalPrimary.dashed}
              aria-hidden="true"
            >
              <span class="terminal-marker">
                {#if terminalPrimary.direction === "download"}
                  {@html ICON.download}
                {:else if terminalPrimary.direction === "upload"}
                  {@html ICON.upload}
                {:else}
                  {@html ICON.bidirectional}
                {/if}
              </span>
              <span class="terminal-number">{terminalPrimary.value}</span>
              <span class="terminal-unit">{gaugeUnit}</span>
              {#if terminalPrimary.dashed}
                <span class="terminal-partial"
                  >Partial {terminalPrimary.direction}</span
                >
              {/if}
            </div>
          {:else}
            {#if display.value}<span class="gauge-value" aria-hidden="true"
                >{display.value}</span
              >{/if}
            {#if display.unit}<span class="gauge-unit" aria-hidden="true"
                >{display.unit}</span
              >{/if}
          {/if}
          <span class="sr-only"
            >{announcementDisplay.value} {announcementDisplay.unit}</span
          >
        </div>
      </div>
      <div class="gauge-footer">
        {#if hint || status || preparationFailure || failNotes.length}
          <div class="gauge-notes">
            {#each failNotes as note (note)}<span class="gauge-fail"
                >{note}</span
              >{/each}
            {#if store.preparing}
              <span class="gauge-status preparation">Checking paths</span>
            {:else if preparationFailure}
              <span class="gauge-status error"
                >{preparationFailure.headline}</span
              >
              <span class="gauge-hint">{preparationFailure.detail}</span>
            {:else if status}
              <span class="gauge-status" class:error={status.tone === "error"}>
                {status.headline}
              </span>
              <span class="gauge-hint">{status.action}</span>
            {:else if hint}<span class="gauge-hint">{hint}</span>{/if}
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
      <div class="latency-panel">
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
  /* Faceplate: the gauge panel is flat and transparent, part of the instrument
     surface. The gauge and latency panels are the wells milled into it
     (--elev-inset), and the controls sit on the faceplate below them. */
  .gauge-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: 0;
    background: transparent;
    /* Query context for .instrument, so a docked panel shrinking this column
       restyles it. It sits here: a container query only styles descendants. */
    container-type: inline-size;
    container-name: viz;
  }
  /* The instrument owns the gauge, profile and controls as one responsive grid. */
  .instrument {
    display: grid;
    gap: var(--space-3) var(--space-2);
    flex: 0 0 auto;
    min-height: 0;
    /* One readable gauge size across live and completed states. The profile
       contributes its intrinsic height instead of acquiring a nested scroller. */
    --gauge-well-height: clamp(280px, 35svh, 360px);
    grid-template:
      "gauge" var(--gauge-well-height)
      "controls" auto
      "latency" auto
      / 1fr;
  }
  /* No latency panel: its row disappears at every width. */
  .instrument:not(:has(.latency-panel)) {
    grid-template:
      "gauge" var(--gauge-well-height)
      "controls" auto
      / 1fr;
  }
  /* Wide instruments pair the two readings and their controls in two columns. */
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
      --gauge-well-height: clamp(360px, 32svh, 420px);
    }
  }
  @media (max-width: 759px) and (orientation: portrait) {
    .instrument {
      /* A phone retains a readable dial while the document carries the results. */
      --gauge-well-height: clamp(280px, 32svh, 320px);
    }
    .instrument .stage {
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
    height: 100%;
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    box-shadow: var(--elev-inset);
    overflow: hidden;
  }
  .gauge-face {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
    /* Size container so the hero number scales with cqmin, the same smaller
       dimension that sizes the ring. cqw overflows a wide, short well. */
    container-type: size;
  }
  .instrument-controls {
    --stage-controls-width: 540px;
    grid-area: controls;
    display: grid;
    gap: var(--space-3);
    width: 100%;
    padding-block: var(--space-2);
    justify-self: center;
    align-items: center;
  }
  .instrument-controls:has(:global(.quad)) {
    --stage-controls-width: 700px;
  }
  @media (max-height: 800px) {
    @container viz (min-width: 1000px) {
      .instrument-controls {
        grid-template-columns: 280px minmax(0, 1fr);
        column-gap: var(--space-5);
        max-width: calc(280px + var(--space-5) + var(--stage-controls-width));
      }
    }
  }
  /* Short wide windows share a control row; taller screens keep the action above its stages. */
  .run-slot {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 46px;
  }
  /* Latency rows remain fully visible; the instrument owns their height. */
  .latency-panel {
    grid-area: latency;
    display: flex;
    flex-direction: column;
    justify-content: center;
    min-width: 240px;
    min-height: 220px;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    box-shadow: var(--elev-inset);
    overflow: visible;
  }
  .gauge-ticks {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .gauge-tick {
    position: absolute;
    transform: translate(-50%, -50%);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: 9.5px;
    font-weight: 600;
    color: var(--text-soft);
    opacity: 0.75;
    white-space: nowrap;
    line-height: 1;
  }
  .gauge-tick[data-anchor-x="end"] {
    transform: translate(-100%, -50%);
  }
  .gauge-tick[data-anchor-x="start"] {
    transform: translate(0, -50%);
  }
  .gauge-tick[data-anchor-y="end"] {
    transform: translate(-50%, -100%);
  }
  .gauge-tick[data-anchor-y="start"] {
    transform: translate(-50%, 0);
  }
  .gauge-tick[data-anchor-x="end"][data-anchor-y="end"] {
    transform: translate(-100%, -100%);
  }
  .gauge-tick[data-anchor-x="end"][data-anchor-y="start"] {
    transform: translate(-100%, 0);
  }
  .gauge-tick[data-anchor-x="start"][data-anchor-y="end"] {
    transform: translate(0, -100%);
  }
  .gauge-tick[data-anchor-x="start"][data-anchor-y="start"] {
    transform: translate(0, 0);
  }
  .metric-wrap {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    /* Keeps the number clear of the gauge ring's sides. The inline padding
       also bounds how wide the value grows until cqmin sizing reins it in. */
    padding-inline: 9%;
    padding-top: calc(2 * var(--gauge-center-offset));
    pointer-events: none;
  }
  /* The hero number: the display face with tabular figures, so a live value
     never shifts layout. Sized in cqmin against the gauge well so large numbers
     shrink to fit a narrow gauge, clamped so it stays legible. */
  .gauge-value {
    font-family: var(--font-display);
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
    font-size: clamp(20px, 14cqmin, 64px);
    font-weight: 600;
    letter-spacing: var(--track-tight);
    color: var(--text);
    line-height: 0.95;
    max-width: 100%;
    white-space: nowrap;
  }
  .terminal-readout {
    --result-accent: var(--text-soft);
    display: grid;
    justify-items: center;
    gap: clamp(var(--space-2), 2.5cqmin, var(--space-3));
    max-width: 72%;
    color: var(--result-accent);
  }
  .terminal-readout.download {
    --result-accent: var(--phase-download);
  }
  .terminal-readout.upload {
    --result-accent: var(--phase-upload);
  }
  .terminal-readout.bidirectional {
    --result-accent: var(--phase-bidirectional);
  }
  .terminal-marker {
    display: grid;
    place-items: center;
    width: clamp(32px, 12cqmin, 42px);
    height: clamp(32px, 12cqmin, 42px);
    border: 1px solid
      color-mix(in srgb, var(--result-accent) 28%, var(--border));
    border-radius: var(--r-full);
    line-height: 1;
  }
  .terminal-marker :global(svg) {
    width: 58%;
    height: 58%;
  }
  .terminal-readout.partial .terminal-marker {
    border-style: dashed;
  }
  .terminal-number {
    font-family: var(--font-display);
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
    font-size: clamp(28px, 14cqmin, 52px);
    font-weight: 600;
    letter-spacing: var(--track-tight);
    line-height: 1;
    white-space: nowrap;
  }
  .terminal-unit {
    font-family: var(--font-mono);
    font-size: clamp(var(--type-xs), 3.8cqmin, var(--type-md));
    font-weight: 500;
    color: var(--text-soft);
    line-height: 1;
  }
  .terminal-partial {
    font-size: var(--type-xs);
    color: var(--text-muted);
  }
  .gauge-unit {
    margin-top: var(--space-1);
    font-family: var(--font-mono);
    font-size: var(--type-sm);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--text-soft);
    /* Unit symbols are case-significant: Mbit/s, kB/s, MiB/s. */
  }
  /* Reserve a separate footer so transient notes cannot overlap the dial. */
  .gauge-footer {
    flex: 0 0 auto;
    min-height: 44px;
    display: grid;
    align-items: center;
    padding: var(--space-2) var(--space-3) var(--space-3);
  }
  .gauge-notes {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    width: 100%;
    text-align: center;
  }
  .gauge-hint {
    font-size: var(--type-sm);
    font-weight: 600;
    line-height: 1.35;
    color: var(--text-muted);
  }
  /* Terminal-state headline (aborted / error) above the softer action line.
     Error is err-tinted. A user abort stays neutral at full text strength, so
     the state is unmissable. */
  .gauge-status {
    font-family: var(--font-mono);
    font-size: var(--type-xs);
    font-weight: 700;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--text);
  }
  .gauge-status.error {
    color: var(--err);
  }
  .gauge-status.preparation {
    color: var(--brand-strong);
  }
  .gauge-fail {
    font-size: var(--type-sm);
    font-weight: 600;
    line-height: 1.3;
    color: var(--err);
  }

  /* Narrow layouts stack the action and stages in their natural reading order. */
  .stage-head {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    width: 100%;
    max-width: var(--stage-controls-width);
    justify-self: center;
  }
  /* The instrument has an explicit well height, so result content no longer
     needs a phantom reserve to keep it stable. Let cards occupy only their
     real height; otherwise the empty reserve becomes a visual gulf above the
     chart. */
  .results-slot {
    flex: 0 0 auto;
    width: 100%;
    max-width: none;
    align-self: center;
    min-height: 0;
  }
  .results-slot:empty {
    display: none;
  }
</style>
