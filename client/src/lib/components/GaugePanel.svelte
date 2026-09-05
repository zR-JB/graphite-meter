<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { GaugeEngine } from "../canvas/GaugeEngine";
  import { watchCanvasPixelRatio } from "../canvas/canvasResolution";
  import { GAUGE_LABEL_FRACTIONS, gaugeLayout } from "../canvas/gaugeLayout";
  import {
    fmtGaugeTick,
    throughputGaugeFraction,
    throughputValueAtFraction,
  } from "../canvas/gaugeScale";
  import StageTrack from "./StageTrack.svelte";
  import RunButton from "./RunButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import ResultCards from "./ResultCards.svelte";
  import { fmtSpeed, fmtMs, reasonLabel } from "../format";
  import { gaugeLatencyPresentation } from "./gaugeLatency";
  import { authoritativeTransferAnnouncement } from "./gaugeAccessibility";
  import {
    LiveRateAnimator,
    type LiveRateValues,
  } from "../presentation/liveRateAnimator";
  import {
    presentation,
    type PresentationHandle,
  } from "../canvas/presentation";
  import { resultGaugeArcs } from "./resultGauge";
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
  // A one-sided bidirectional partial retains its lane result for diagnostics,
  // but has no truthful combined gauge value.
  const unusableStage = $derived(
    activeStagePresentation?.status === "failed" ||
      (store.phase === "complete" &&
        terminalArcs.length === 0 &&
        !store.result?.latency),
  );

  let canvasEl = $state<HTMLCanvasElement>();
  let stageEl = $state<HTMLDivElement>();
  let engine: GaugeEngine;
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
      if (terminalArcs.length === 1)
        return {
          value: fmtSpeed(gaugeRate(terminalArcs[0].bytesPerSec)),
          unit: `${gaugeUnit} · ${terminalArcs[0].label}`,
        };
      if (terminalArcs.length > 1) return { value: "", unit: gaugeUnit };
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
    const active =
      store.measuring &&
      (phase === "download" || phase === "upload" || phase === "bidirectional");
    const context = `${store.runSeq}:${phase}`;
    const bidi = store.visualBidirectional ?? { down: 0, up: 0 };
    return {
      active,
      context,
      transfer: {
        target: store.visualTransferBytesPerSec,
        revision: store.presentationRateRevision.transfer,
      },
      down: {
        target: bidi.down,
        revision: store.presentationRateRevision.down,
      },
      up: {
        target: bidi.up,
        revision: store.presentationRateRevision.up,
      },
    };
  });

  function stepLiveRates(now: number): boolean {
    const input = liveRateInput;
    const transfer = liveRateAnimator.step(
      {
        key: "transfer",
        target: input.transfer.target,
        revision: input.transfer.revision,
        context: input.context,
        active: input.active,
      },
      now,
      reducedRateMotion,
    );
    const down = liveRateAnimator.step(
      {
        key: "down",
        target: input.down.target,
        revision: input.down.revision,
        context: input.context,
        active: input.active,
      },
      now,
      reducedRateMotion,
    );
    const up = liveRateAnimator.step(
      {
        key: "up",
        target: input.up.target,
        revision: input.up.revision,
        context: input.context,
        active: input.active,
      },
      now,
      reducedRateMotion,
    );
    liveRateValues = {
      transfer: transfer.value,
      down: down.value,
      up: up.value,
    };
    return transfer.active || down.active || up.active;
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
      return authoritativeTransferAnnouncement({
        authoritativeBytesPerSec: store.liveTransferBytesPerSec,
        visualBytesPerSec: store.visualTransferBytesPerSec,
        toUnit: gaugeRate,
        unit: gaugeUnit,
      });
    return display;
  });

  // The hero value may ease toward a presentation-only upload hint. Keep its
  // visible text out of the accessibility tree; assistive technology receives
  // the same authoritative value used by the live announcement instead.
  const accessibleDisplay = $derived(announcementDisplay);
  const terminalAnnouncement = $derived(
    terminalArcs
      .map(
        (arc) =>
          `${arc.label} ${fmtSpeed(gaugeRate(arc.bytesPerSec))} ${gaugeUnit}${arc.dashed ? ", partial" : ""}`,
      )
      .join("; "),
  );
  const terminalSummary = $derived.by(() =>
    store.phase === "complete" && terminalArcs.length > 1
      ? terminalArcs.map((arc) => ({
          value: fmtSpeed(gaugeRate(arc.bytesPerSec)),
          direction:
            arc.phase === "download" || arc.label.endsWith("download")
              ? "download"
              : arc.phase === "upload" || arc.label.endsWith("upload")
                ? "upload"
                : "bidirectional",
          phase: arc.phase,
          dashed: arc.dashed,
        }))
      : [],
  );

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

  // Wake the gauge loop for exactly the state GaugeEngine reads. The loop parks
  // once a run settles, so untracked state leaves the dial frozen.
  $effect(() => {
    void store.phase;
    void store.throughput.length;
    void store.latency.length;
    void gaugeLatency.rttMs;
    void gaugeLatency.scaleMs;
    void store.liveLatencyLost;
    void store.gaugeScaleBytesPerSec;
    void store.liveThroughput.length;
    void throughputEvidence;
    void store.measuring;
    void store.unitBase;
    void store.unitKind;
    void gaugeUnit;
    void liveRateValues;
    void layout;
    engine?.wake();
  });

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

  onMount(() => {
    engine = new GaugeEngine(() => {
      const p = store.phase;
      const scale = store.gaugeScaleBytesPerSec;
      return {
        phase: p,
        showValue: !unusableStage,
        valueBytesPerSec: unusableStage
          ? 0
          : p === "complete" && terminalArcs.length
            ? terminalArcs[0].bytesPerSec
            : liveRateValues.transfer,
        scaleBytesPerSec: scale,
        throughputEvidence:
          p === "complete" ? terminalArcs.length > 0 : throughputEvidence,
        latencyScaleMs: gaugeLatency.scaleMs,
        layout,
        rtt: gaugeLatency.rttMs,
        completedKind,
        resultArcs:
          p === "complete"
            ? terminalArcs.map((arc) => ({
                phase: arc.phase,
                fraction: throughputGaugeFraction(arc.bytesPerSec, scale),
                dashed: arc.dashed,
              }))
            : [],
      };
    });
    engine.attach(canvasEl!);
    liveRatePresentation = presentation.register(stageEl!, stepLiveRates);
    const rateMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedRateMotion = rateMotion.matches;
    const onRateMotion = (event: MediaQueryListEvent) => {
      reducedRateMotion = event.matches;
      liveRatePresentation?.invalidate();
    };
    rateMotion.addEventListener("change", onRateMotion);
    const themeObserver = new MutationObserver(() => engine.invalidateTheme());
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });

    const resizeObserver = new ResizeObserver((entries) => {
      const { width, height } = entries[0].contentRect;
      if (gaugeSize.width !== width || gaugeSize.height !== height)
        gaugeSize = { width, height };
      engine.resize(width, height);
    });
    resizeObserver.observe(stageEl!);
    const { clientWidth: width, clientHeight: height } = stageEl!;
    gaugeSize = { width, height };
    engine.resize(width, height);
    const stopWatchingPixelRatio = watchCanvasPixelRatio(() =>
      engine.resize(stageEl!.clientWidth, stageEl!.clientHeight),
    );

    return () => {
      if (announceTimer) clearTimeout(announceTimer);
      rateMotion.removeEventListener("change", onRateMotion);
      liveRatePresentation?.destroy();
      liveRatePresentation = null;
      liveRateAnimator.reset();
      engine.destroy();
      themeObserver.disconnect();
      resizeObserver.disconnect();
      stopWatchingPixelRatio();
    };
  });
</script>

<section class="gauge-panel">
  <!-- One container-query grid switches the complete instrument layout and
       keeps the gauge track stable when the latency panel is toggled. -->
  <div class="instrument">
    <div bind:this={stageEl} class="stage">
      <canvas bind:this={canvasEl} class="canvas" aria-hidden="true"></canvas>
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
        {#if terminalSummary.length}
          <div class="terminal-readout" aria-hidden="true">
            <div class="terminal-summary">
              {#each terminalSummary as item (item.phase)}
                <span
                  class="terminal-result {item.phase}"
                  class:partial={item.dashed}
                >
                  <span class="terminal-marker">
                    {#if item.direction === "download"}
                      {@html ICON.download}
                    {:else if item.direction === "upload"}
                      {@html ICON.upload}
                    {:else}
                      {@html ICON.bidirectional}
                    {/if}
                  </span>
                  <span class="terminal-number">{item.value}</span>
                </span>
              {/each}
            </div>
            <span class="terminal-unit">{display.unit}</span>
          </div>
        {/if}
        {#if display.value}<span class="gauge-value" aria-hidden="true"
            >{display.value}</span
          >{/if}
        {#if display.unit && !terminalSummary.length}<span
            class="gauge-unit"
            aria-hidden="true">{display.unit}</span
          >{/if}
        <span class="sr-only"
          >{accessibleDisplay.value} {accessibleDisplay.unit}</span
        >
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

    <div class="run-slot"><RunButton /></div>

    <div class="stage-head"><StageTrack /></div>

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
    gap: var(--space-2);
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
    gap: var(--space-2);
    flex: 0 0 auto;
    min-height: 0;
    /* One readable gauge size across live and completed states. The profile
       contributes its intrinsic height instead of acquiring a nested scroller. */
    --gauge-well-height: clamp(280px, 35svh, 360px);
    grid-template:
      "gauge" var(--gauge-well-height)
      "run" auto
      "stagehead" auto
      "latency" auto
      / 1fr;
  }
  /* No latency panel: its row disappears at every width. */
  .instrument:not(:has(.latency-panel)) {
    grid-template:
      "gauge" var(--gauge-well-height)
      "run" auto
      "stagehead" auto
      / 1fr;
  }
  /* Wide instruments pair the two readings and their controls in two columns. */
  @container viz (min-width: 760px) {
    .instrument {
      grid-template:
        "gauge latency" minmax(var(--gauge-well-height), auto)
        "run run" auto
        "stagehead stagehead" auto
        / minmax(240px, 1fr) minmax(240px, 1fr);
    }
    /* The gauge keeps its size when latency is disabled. */
    .instrument:not(:has(.latency-panel)) {
      grid-template:
        "gauge gauge" var(--gauge-well-height)
        "run run" auto
        "stagehead stagehead" auto
        / minmax(240px, 1fr) minmax(240px, 1fr);
    }
  }
  @media (max-width: 759px) and (orientation: portrait) {
    .instrument {
      /* A phone retains a readable dial while the document carries the results. */
      --gauge-well-height: clamp(240px, 30svh, 300px);
    }
    .instrument .stage {
      min-height: 240px;
    }
    .canvas,
    .gauge-ticks,
    .metric-wrap {
      /* The 270-degree dial is top-heavy by construction. A small optical
         shift balances its label clearance in the compact phone well. */
      transform: translateY(8px);
    }
  }
  /* The gauge well: the deepest recess on the faceplate. */
  .stage {
    grid-area: gauge;
    position: relative;
    min-width: 240px;
    min-height: 220px;
    height: 100%;
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    box-shadow: var(--elev-inset);
    overflow: hidden;
    /* Size container so the hero number scales with cqmin, the same smaller
       dimension that sizes the ring. cqw overflows a wide, short well. */
    container-type: size;
  }
  /* Start test's slot: RunButton centers itself (width:100%, max-width:320px,
     align-self:center), so this slot only has to be a flex row. */
  .run-slot {
    grid-area: run;
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
  .canvas {
    position: absolute;
    inset: 0;
    display: block;
    width: 100%;
    height: 100%;
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
    font-size: 8.5px;
    font-weight: 600;
    color: var(--text-soft);
    opacity: 0.5;
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
    display: grid;
    justify-items: center;
    gap: 8px;
    max-width: 72%;
  }
  .terminal-summary {
    display: grid;
    gap: 4px;
    max-width: 100%;
    font-family: var(--font-display);
    font-variant-numeric: tabular-nums;
    font-feature-settings: "tnum" 1;
    font-size: clamp(18px, 7.2cqmin, 30px);
    font-weight: 600;
    letter-spacing: var(--track-tight);
    line-height: 1;
    white-space: nowrap;
  }
  .terminal-result {
    --result-accent: var(--text-soft);
    display: grid;
    grid-template-columns: 21px minmax(4ch, 1fr);
    align-items: center;
    gap: 7px;
  }
  .terminal-result.download {
    --result-accent: var(--phase-download);
  }
  .terminal-result.upload {
    --result-accent: var(--phase-upload);
  }
  .terminal-result.bidirectional {
    --result-accent: var(--phase-bidirectional);
  }
  .terminal-marker {
    display: grid;
    place-items: center;
    width: 21px;
    height: 21px;
    border: 1px solid
      color-mix(in srgb, var(--result-accent) 30%, var(--border));
    border-radius: var(--r-well);
    background: color-mix(in srgb, var(--result-accent) 6%, var(--surface-2));
    color: var(--result-accent);
    line-height: 1;
  }
  .terminal-marker :global(svg) {
    width: 13px;
    height: 13px;
  }
  .terminal-result.partial .terminal-marker {
    border-style: dashed;
  }
  .terminal-number {
    min-width: 0;
    text-align: right;
    color: var(--result-accent);
  }
  .terminal-unit {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 7px;
    width: 100%;
    font-family: var(--font-mono);
    font-size: clamp(10px, 3.1cqmin, 12px);
    font-weight: 600;
    letter-spacing: 0.07em;
    color: var(--text-soft);
    line-height: 1;
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
  @media (prefers-reduced-motion: no-preference) {
    .terminal-result {
      animation: terminal-result-enter var(--dur-slide) var(--ease-out) both;
    }
    .terminal-result:nth-child(2) {
      animation-delay: 35ms;
    }
    .terminal-result:nth-child(3) {
      animation-delay: 70ms;
    }
    .terminal-unit {
      animation: terminal-unit-enter var(--dur-hover) var(--ease-out)
        var(--dur-hover) both;
    }
  }
  @keyframes terminal-result-enter {
    from {
      opacity: 0;
      transform: translateY(3px);
    }
  }
  @keyframes terminal-unit-enter {
    from {
      opacity: 0;
    }
  }
  /* Notes zone at the dial's foot: guided idle/transient copy and
     skipped-stage explanations. Centered beneath the big metric; doesn't affect
     the metric's zero-shift baseline. */
  .gauge-notes {
    position: absolute;
    bottom: 18px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 86%;
    text-align: center;
  }
  .gauge-hint {
    font-size: 12.5px;
    font-weight: 600;
    line-height: 1.35;
    color: var(--text-muted);
  }
  /* Terminal-state headline (aborted / error) above the softer action line.
     Error is err-tinted. A user abort stays neutral at full text strength, so
     the state is unmissable. */
  .gauge-status {
    font-family: var(--font-mono);
    font-size: 11px;
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
    font-size: 11.5px;
    font-weight: 600;
    line-height: 1.3;
    color: var(--err);
  }

  /* Stage-head block: Test Stages header plus track, placed by the instrument
     grid (top on mobile, bottom on desktop). Capped to a comfortable measure
     and centered so it never stretches across a full two-column span. */
  .stage-head {
    grid-area: stagehead;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    width: 100%;
    max-width: 600px;
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
</style>
