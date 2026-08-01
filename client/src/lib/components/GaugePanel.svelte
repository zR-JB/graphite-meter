<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { GaugeEngine } from "../canvas/GaugeEngine";
  import StageTrack from "./StageTrack.svelte";
  import RunButton from "./RunButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import ResultCards from "./ResultCards.svelte";
  import { fmtSpeed, fmtMs, reasonLabel } from "../format";
  import { tooltip } from "../actions/tooltip";
  import { presentWireEstimate } from "../wirePresentation";

  const resultsView = $derived.by<"none" | "partial" | "final">(() => {
    if (store.phase === "complete") return "final";
    if (store.phase === "idle") return "none";
    return "partial";
  });

  let canvasEl = $state<HTMLCanvasElement>();
  let engine: GaugeEngine;

  const TICK_FRACTIONS = [0, 0.25, 0.5, 0.75, 1];
  const EMPTY_DISPLAY = { value: "—", unit: "" };
  let completedDisplay = $state(EMPTY_DISPLAY);
  let completedKind = $state<"speed" | "latency">("speed");

  $effect(() => {
    if (store.phase === "latency") {
      completedKind = "latency";
      completedDisplay = store.liveLatencyLost
        ? { value: "lost", unit: "" }
        : { value: fmtMs(store.liveRtt), unit: "ms" };
    } else if (
      store.phase === "download" ||
      store.phase === "upload" ||
      store.phase === "bidirectional"
    ) {
      completedKind = "speed";
      completedDisplay = {
        value: fmtSpeed(store.toUnit(store.liveTransferBytesPerSec)),
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
      completedDisplay = { value: fmtMs(metric.ms), unit: "ms" };
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
      return TICK_FRACTIONS.map((f) => fmtMs(store.latencyScaleMs * f));
    const scale = store.displayScaleBytesPerSec;
    return TICK_FRACTIONS.map((f) => fmtSpeed(store.toUnit(scale * f)));
  });

  const display = $derived.by(() => {
    const p = store.phase;
    if (p === "latency")
      return store.liveLatencyLost
        ? { value: "lost", unit: "" }
        : { value: fmtMs(store.liveRtt), unit: "ms" };
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
      value: fmtSpeed(store.toUnit(store.liveTransferBytesPerSec)),
      unit: store.unitLabel,
    };
  });

  const wire = $derived.by(() => {
    if (!store.showWireEstimates || completedKind === "latency") return null;
    const estimate =
      store.phase === "complete"
        ? store.finalCompensation
        : store.phase === "bidirectional"
          ? store.liveBidirectionalCompensation
          : store.phase === "download" || store.phase === "upload"
            ? store.liveCompensation
            : null;
    if (!estimate?.available || estimate.measuredBytesPerSec <= 0) return null;
    return presentWireEstimate(
      estimate,
      (bytesPerSec) =>
        `${fmtSpeed(store.toUnit(bytesPerSec))} ${store.unitLabel}`,
    );
  });

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
    void store.liveRtt;
    void store.liveLatencyLost;
    void store.displayScaleBytesPerSec;
    void store.measuring;
    void store.unitBase;
    void store.unitKind;
    void store.unitLabel;
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
      statusText || `${display.value} ${display.unit}, phase ${phase}`;
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
        valueBytesPerSec:
          finalMetric?.kind === "speed"
            ? finalMetric.bytesPerSec
            : store.liveTransferBytesPerSec,
        scaleBytesPerSec: scale,
        latencyScaleMs: store.latencyScaleMs,
        ticks: gaugeTicks,
        rtt: finalMetric?.kind === "latency" ? finalMetric.ms : store.liveRtt,
        completedKind,
      };
    });
    engine.attach(canvasEl!);
    const themeObserver = new MutationObserver(() => engine.invalidateTheme());
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });

    const resizeObserver = new ResizeObserver(() => engine.invalidateTheme());
    resizeObserver.observe(canvasEl!);

    return () => {
      if (announceTimer) clearTimeout(announceTimer);
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

    <div class="stage">
      <canvas bind:this={canvasEl} class="canvas" aria-hidden="true"></canvas>
      <div class="metric-wrap">
        <span class="gauge-value">{display.value}</span>
        {#if display.unit}<span class="gauge-unit">{display.unit}</span>{/if}
        {#if wire?.kind === "estimate"}
          <span class="gauge-wire" use:tooltip={wire.tooltip}>{wire.text}</span>
        {/if}
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
      <ResultCards compact />
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
  /* The instrument grid places stage-head, gauge, Engage, and the optional
     latency panel, so one breakpoint flips the whole arrangement. Its
     gauge+latency track is content-independent: the latency panel scrolls
     inside its own height. */
  .instrument {
    display: grid;
    gap: var(--space-3);
    flex: 1 1 auto;
    min-height: 0;
    grid-template:
      "stagehead" auto
      "gauge" minmax(220px, 1fr)
      "engage" auto
      "latency" auto
      / 1fr;
  }
  /* No latency panel: its row disappears at every width. */
  .instrument:not(:has(.latency-panel)) {
    grid-template:
      "stagehead" auto
      "gauge" minmax(220px, 1fr)
      "engage" auto
      / 1fr;
  }
  /* Wide: gauge + latency side-by-side (each min-width:240px + gap ≈ 492px;
     520px leaves a safety margin over the columns' min-width floor). One
     query moves Engage, the latency panel, and Test Stages together. */
  @container viz (min-width: 520px) {
    .instrument {
      grid-template:
        "gauge latency" minmax(220px, 1fr)
        "engage engage" auto
        "stagehead stagehead" auto
        / minmax(240px, 1fr) minmax(240px, 1fr);
    }
    .instrument:not(:has(.latency-panel)) {
      grid-template:
        "gauge" minmax(220px, 1fr)
        "engage" auto
        "stagehead" auto
        / 1fr;
    }
  }
  /* The gauge well: the deepest recess on the faceplate. */
  .stage {
    grid-area: gauge;
    position: relative;
    min-width: 240px;
    min-height: 220px;
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
  .gauge-wire {
    margin-top: var(--space-1);
    max-width: 82%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: clamp(10px, 3.4cqmin, 13px);
    color: var(--text-muted);
    pointer-events: auto;
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

  /* Results slot: empty at idle, a compact strip mid-run, the full card grid
     once complete. The min-height reserve holds in every state, so the gauge
     above keeps one size. 760px fits 4 cards in one row (4x181px + 3x12px
     gap); the 600px measure used above wraps them 3-then-1. */
  .results-slot {
    width: 100%;
    max-width: 760px;
    align-self: center;
    min-height: 108px;
  }
  /* Stacked: the document scrolls, so the reserve only reads as dead space
     between the controls and the chart. Collapsed here, results push the
     chart down when they appear. */
  @media (max-width: 759px) {
    /* bp: stacked */
    .results-slot {
      min-height: 0;
    }
  }
</style>
