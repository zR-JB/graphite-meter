<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { GaugeEngine } from "../canvas/GaugeEngine";
  import { gaugeLayout } from "../canvas/gaugeLayout";
  import StageTrack from "./StageTrack.svelte";
  import RunButton from "./RunButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import ResultCards from "./ResultCards.svelte";
  import { fmtSpeed, fmtMs, reasonLabel } from "../format";
  import { tooltip } from "../actions/tooltip";
  import { gaugeLatencyPresentation } from "./gaugeLatency";
  import { authoritativeTransferAnnouncement } from "./gaugeAccessibility";
  import {
    LiveRateAnimator,
    type LiveRateValues,
  } from "../presentation/liveRateAnimator";

  const resultsView = $derived.by<"none" | "partial" | "final">(() => {
    if (store.phase === "complete") return "final";
    if (store.phase === "idle") return "none";
    return "partial";
  });
  const activeStagePresentation = $derived(
    store.phaseStage ? store.stagePresentation[store.phaseStage] : null,
  );
  // A one-sided bidirectional partial retains its lane result for diagnostics,
  // but has no truthful combined gauge value.
  const unusableStage = $derived(
    activeStagePresentation?.status === "failed" ||
      (store.phase === "complete" && store.finalMetric === null),
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
  let liveRateFrame = 0;
  let reducedRateMotion = false;

  const TICK_FRACTIONS = [0, 0.25, 0.5, 0.75, 1];
  const EMPTY_DISPLAY = { value: "—", unit: "" };
  let completedDisplay = $state(EMPTY_DISPLAY);
  let completedKind = $state<"speed" | "latency">("speed");
  const gaugeLatency = $derived.by(() => {
    const metric = store.phase === "complete" ? store.finalMetric : null;
    return gaugeLatencyPresentation({
      phase: store.phase,
      liveRttMs: store.liveRtt,
      liveScaleMs: store.latencyScaleMs,
      history: store.latency,
      completedRttMs: metric?.kind === "latency" ? metric.ms : null,
    });
  });

  $effect(() => {
    if (store.phase === "latency") {
      completedKind = "latency";
      completedDisplay = store.liveLatencyLost
        ? { value: "lost", unit: "" }
        : { value: fmtMs(gaugeLatency.rttMs), unit: "ms" };
    } else if (
      store.phase === "download" ||
      store.phase === "upload" ||
      store.phase === "bidirectional"
    ) {
      completedKind = "speed";
      completedDisplay = {
        value: fmtSpeed(store.toUnit(store.visualTransferBytesPerSec)),
        unit: store.unitLabel,
      };
    }
  });

  $effect(() => {
    if (store.phase !== "complete") return;
    const metric = store.finalMetric;
    if (!metric) return;
    if (metric.kind === "latency") {
      completedKind = "latency";
      completedDisplay = { value: fmtMs(gaugeLatency.rttMs), unit: "ms" };
    } else {
      completedKind = "speed";
      completedDisplay = {
        value: fmtSpeed(store.toUnit(metric.bytesPerSec)),
        unit: store.unitLabel,
      };
    }
  });

  const msTicksActive = $derived(
    store.phase === "latency" ||
      (store.phase === "complete" && completedKind === "latency"),
  );
  const gaugeTicks = $derived.by(() => {
    if (msTicksActive)
      return TICK_FRACTIONS.map((f) => fmtMs(gaugeLatency.scaleMs * f));
    const scale = store.displayScaleBytesPerSec;
    return TICK_FRACTIONS.map((f) => fmtSpeed(store.toUnit(scale * f)));
  });
  const layout = $derived.by(() =>
    gaugeLayout(gaugeSize.width, gaugeSize.height, gaugeTicks.length),
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
        ? { value: "lost", unit: "" }
        : { value: fmtMs(gaugeLatency.rttMs), unit: "ms" };
    if (
      p === "idle" ||
      p === "connecting" ||
      p === "error" ||
      p === "aborted" ||
      p === "warmup"
    )
      return EMPTY_DISPLAY;
    if (p === "complete") return completedDisplay;
    return {
      value: fmtSpeed(store.toUnit(liveRateValues.transfer)),
      unit: store.unitLabel,
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

  function stepLiveRates(now: number) {
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
    if (transfer.active || down.active || up.active)
      liveRateFrame = requestAnimationFrame(stepLiveRates);
    else liveRateFrame = 0;
  }

  function wakeLiveRates() {
    if (!liveRateFrame) liveRateFrame = requestAnimationFrame(stepLiveRates);
  }

  $effect(() => {
    void liveRateInput;
    wakeLiveRates();
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
        toUnit: store.toUnit.bind(store),
        unit: store.unitLabel,
      });
    return display;
  });

  // The hero value may ease toward a presentation-only upload hint. Keep its
  // visible text out of the accessibility tree; assistive technology receives
  // the same authoritative value used by the live announcement instead.
  const accessibleDisplay = $derived(announcementDisplay);

  const STAGE_NAME: Record<string, string> = {
    latency: "Latency",
    download: "Download",
    upload: "Upload",
    bidirectional: "Bidirectional",
  };
  const failNotes = $derived(
    store.transferFailures.map(
      (f) => `${STAGE_NAME[f.stage]} skipped — ${f.message}`,
    ),
  );

  const hint = $derived.by(() => {
    switch (store.phase) {
      case "idle":
        return "Press Engage to start your speed test";
      case "connecting":
        return "Verifying the selected protocol…";
      case "warmup":
        return "Checking your connection…";
      default:
        return "";
    }
  });

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
    status ? `${status.headline} — ${status.action}` : hint,
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
    void store.displayScaleBytesPerSec;
    void store.measuring;
    void store.unitBase;
    void store.unitKind;
    void store.unitLabel;
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
      const scale = store.displayScaleBytesPerSec;
      const finalMetric = p === "complete" ? store.finalMetric : null;
      return {
        phase: p,
        showValue: !unusableStage,
        valueBytesPerSec: unusableStage
          ? 0
          : finalMetric?.kind === "speed"
            ? finalMetric.bytesPerSec
            : liveRateValues.transfer,
        scaleBytesPerSec: scale,
        latencyScaleMs: gaugeLatency.scaleMs,
        layout,
        rtt: gaugeLatency.rttMs,
        completedKind,
      };
    });
    engine.attach(canvasEl!);
    const rateMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedRateMotion = rateMotion.matches;
    const onRateMotion = (event: MediaQueryListEvent) => {
      reducedRateMotion = event.matches;
      wakeLiveRates();
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

    return () => {
      if (announceTimer) clearTimeout(announceTimer);
      if (liveRateFrame) cancelAnimationFrame(liveRateFrame);
      rateMotion.removeEventListener("change", onRateMotion);
      liveRateAnimator.reset();
      engine.destroy();
      themeObserver.disconnect();
      resizeObserver.disconnect();
    };
  });
</script>

<section class="gauge-panel">
  <!-- One container-query grid switches the complete instrument layout and
       keeps the gauge track stable when the latency panel is toggled. -->
  <div class="instrument">
    <div class="stage-head">
      <div class="controls-head">
        <span class="controls-title">Test stages</span>
        <span
          class="eta"
          use:tooltip={"Estimated run time at the saved duration"}
        >
          ~{(store.totalEtaMs / 1000).toFixed(0)}s
        </span>
      </div>
      <StageTrack />
    </div>

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
              style:top={`${point.y}px`}>{gaugeTicks[index]}</span
            >
          {/each}
        </div>
      {/if}
      <div class="metric-wrap">
        <span class="gauge-value" aria-hidden="true">{display.value}</span>
        {#if display.unit}<span class="gauge-unit" aria-hidden="true"
            >{display.unit}</span
          >{/if}
        <span class="sr-only"
          >{accessibleDisplay.value} {accessibleDisplay.unit}</span
        >
        {#if hint || status || failNotes.length}
          <div class="gauge-notes">
            {#each failNotes as note (note)}<span class="gauge-fail"
                >{note}</span
              >{/each}
            {#if status}
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

    <div class="engage-slot"><RunButton /></div>

    {#if store.latencyEnabled}
      <div class="latency-panel">
        <LatencyProfile bare />
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
    /* Result content sits below the instrument. Its actual occupied height is
       reserved from the shared gauge well through CSS, never through a
       resize-observer/viewport feedback loop. */
    --result-slot-budget: 0px;
  }
  .gauge-panel:has(:global(.result-chip:nth-child(1))) {
    --result-slot-budget: 36px;
  }
  .gauge-panel:has(:global(.result-chip:nth-child(2))) {
    --result-slot-budget: 72px;
  }
  .gauge-panel:has(:global(.result-chip:nth-child(3))) {
    --result-slot-budget: 108px;
  }
  .gauge-panel:has(:global(.result-chip:nth-child(4))) {
    --result-slot-budget: 144px;
  }
  .gauge-panel:has(:global(.result-card)) {
    --result-slot-budget: 80px;
  }
  /* The instrument grid places stage-head, gauge, Engage, and the optional
     latency panel, so one breakpoint flips the whole arrangement. Its
     gauge+latency track is content-independent: the latency panel scrolls
     inside its own height. */
  .instrument {
    display: grid;
    gap: var(--space-3);
    flex: 0 0 auto;
    min-height: 0;
    /* The stacked mode has intrinsic wells and participates in document flow.
       Its size is independent of the optional latency row. */
    /* The shared well is capped by the fixed shell, controls, rail, chart
       floor, and their gaps. On a windowed desktop it yields before the stage
       grows a token scrollbar; taller viewports retain the generous 42% well.
       This remains independent of the optional latency panel. */
    --instrument-stage-reserve: 458px;
    --gauge-well-height: clamp(
      220px,
      min(
        42svh,
        calc(
          100dvh - var(--instrument-stage-reserve) - var(--result-slot-budget)
        )
      ),
      360px
    );
    grid-template:
      "stagehead" auto
      "gauge" var(--gauge-well-height)
      "engage" auto
      "latency" auto
      / 1fr;
  }
  /* No latency panel: its row disappears at every width. */
  .instrument:not(:has(.latency-panel)) {
    grid-template:
      "stagehead" auto
      "gauge" var(--gauge-well-height)
      "engage" auto
      / 1fr;
  }
  /* Wide: gauge + latency side-by-side (each min-width:240px + gap ≈ 492px;
     520px leaves a safety margin over the columns' min-width floor). One
     query moves Engage, the latency panel, and Test Stages together. */
  @container viz (min-width: 520px) {
    .instrument {
      grid-template:
        "gauge latency" var(--gauge-well-height)
        "engage engage" auto
        "stagehead stagehead" auto
        / minmax(240px, 1fr) minmax(240px, 1fr);
    }
    /* Keep the gauge in the same dedicated column when latency is disabled.
       A missing optional panel must not resize its CSS-pixel geometry. */
    .instrument:not(:has(.latency-panel)) {
      grid-template:
        "gauge ." var(--gauge-well-height)
        "engage engage" auto
        "stagehead stagehead" auto
        / minmax(240px, 1fr) minmax(240px, 1fr);
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
  /* Engage's slot: RunButton centers itself (width:100%, max-width:320px,
     align-self:center), so this slot only has to be a flex row. */
  .engage-slot {
    grid-area: engage;
    display: flex;
    justify-content: center;
  }
  /* Latency profile: a matching engraved well, sized identically to the gauge
     so the pair reads as one balanced instrument. Its content scrolls within
     the shared height rather than forcing the row taller. */
  .latency-panel {
    grid-area: latency;
    min-width: 240px;
    min-height: 220px;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    box-shadow: var(--elev-inset);
    overflow: auto;
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
    font-size: 8.5px;
    font-weight: 600;
    color: var(--text-soft);
    opacity: 0.5;
    white-space: nowrap;
    line-height: 1;
  }
  .gauge-tick[data-anchor-x="start"] {
    transform: translate(0, -50%);
  }
  .gauge-tick[data-anchor-x="end"] {
    transform: translate(-100%, -50%);
  }
  .gauge-tick[data-anchor-y="start"] {
    transform: translate(-50%, 0);
  }
  .gauge-tick[data-anchor-y="end"] {
    transform: translate(-50%, -100%);
  }
  .gauge-tick[data-anchor-x="start"][data-anchor-y="start"] {
    transform: translate(0, 0);
  }
  .gauge-tick[data-anchor-x="start"][data-anchor-y="end"] {
    transform: translate(0, -100%);
  }
  .gauge-tick[data-anchor-x="end"][data-anchor-y="start"] {
    transform: translate(-100%, 0);
  }
  .gauge-tick[data-anchor-x="end"][data-anchor-y="end"] {
    transform: translate(-100%, -100%);
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
  .gauge-unit {
    margin-top: var(--space-1);
    font-family: var(--font-mono);
    font-size: var(--type-sm);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--text-soft);
    /* Unit symbols are case-significant: Mbit/s, kB/s, MiB/s. */
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
  .controls-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  .controls-title {
    font-size: var(--type-xs);
    font-weight: 700;
    letter-spacing: var(--track-wide);
    text-transform: uppercase;
    color: var(--text-soft);
  }
  .eta {
    font-family: var(--font-mono);
    font-size: var(--type-sm);
    font-weight: 700;
    color: var(--text-muted);
    letter-spacing: 0;
  }

  /* The instrument has an explicit well height, so result content no longer
     needs a phantom reserve to keep it stable. Let cards occupy only their
     real height; otherwise the empty reserve becomes a visual gulf above the
     chart. */
  .results-slot {
    flex: 0 0 auto;
    width: 100%;
    max-width: 760px;
    align-self: center;
    min-height: 0;
  }
</style>
