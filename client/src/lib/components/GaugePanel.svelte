<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { GaugeEngine } from "../canvas/GaugeEngine";
  import {
    presentation,
    type PresentationHandle,
  } from "../canvas/presentation";
  import StageTrack from "./StageTrack.svelte";
  import RunButton from "./RunButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import ResultCards from "./ResultCards.svelte";
  import { fmtSpeed, fmtMs, reasonLabel } from "../format";
  import { tooltip } from "../actions/tooltip";

  const resultsView = $derived.by<"none" | "partial" | "final">(() => {
    if (store.phase === "complete") return "final";
    if (store.phase === "idle") return "none";
    return "partial";
  });

  let canvasEl = $state<HTMLCanvasElement>();
  let stageEl = $state<HTMLDivElement>();
  let engine: GaugeEngine;
  let decayPresentation: PresentationHandle;

  const STALL_DECAY_MS = 800;
  const TICK_FRACTIONS = [0, 0.25, 0.5, 0.75, 1];
  const EMPTY_DISPLAY = { value: "—", unit: "" };
  let nowWall = $state(performance.now());
  const stallDecay = $derived.by(() => {
    if (store.measuring || !store.stalledSince) return 1;
    return Math.min(
      1,
      Math.max(0, 1 - (nowWall - store.stalledSince) / STALL_DECAY_MS),
    );
  });
  const decayedBytesPerSec = $derived(
    store.liveTransferBytesPerSec * stallDecay,
  );
  let completedDisplay = $state(EMPTY_DISPLAY);
  let completedKind = $state<"speed" | "latency">("speed");

  $effect(() => {
    if (store.phase === "latency") {
      completedKind = "latency";
      completedDisplay = { value: fmtMs(store.liveRtt), unit: "ms" };
    } else if (
      store.phase === "download" ||
      store.phase === "upload" ||
      store.phase === "bidirectional"
    ) {
      completedKind = "speed";
      completedDisplay = {
        value: fmtSpeed(store.toUnit(decayedBytesPerSec)),
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

  const LATENCY_SCALE_LADDER = [20, 40, 100, 200, 400, 1000, 2000, 4000];

  const latencyScaleMs = $derived.by(() => {
    let peak = store.infra?.preTestPingMs ?? 0;
    for (const s of store.latency)
      if (!s.lost && s.rttMs > peak) peak = s.rttMs;
    const target = peak * 1.1; // a touch of headroom so the peak isn't pegged
    return (
      LATENCY_SCALE_LADDER.find((step) => step >= target) ??
      LATENCY_SCALE_LADDER.at(-1)!
    );
  });

  const msTicksActive = $derived(
    store.phase === "latency" ||
      (store.phase === "complete" && completedKind === "latency"),
  );
  const gaugeTicks = $derived.by(() => {
    if (msTicksActive)
      return TICK_FRACTIONS.map((f) => fmtMs(latencyScaleMs * f));
    const scale = store.displayScaleBytesPerSec;
    return TICK_FRACTIONS.map((f) => fmtSpeed(store.toUnit(scale * f)));
  });

  const display = $derived.by(() => {
    const p = store.phase;
    if (p === "latency") return { value: fmtMs(store.liveRtt), unit: "ms" };
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
      value: fmtSpeed(store.toUnit(decayedBytesPerSec)),
      unit: store.unitLabel,
    };
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

  // Wake the gauge loop for exactly the state GaugeEngine reads: it parks once a
  // run settles, so anything untracked here would leave the dial frozen.
  $effect(() => {
    void store.phase;
    void store.throughput.length;
    void store.latency.length;
    void store.liveRtt;
    void store.displayScaleBytesPerSec;
    void store.measuring;
    void store.unitBase;
    void store.unitKind;
    void store.unitLabel;
    engine?.wake();
  });

  $effect(() => {
    void store.measuring;
    void store.stalledSince;
    decayPresentation?.invalidate();
  });

  // Returns whether the scheduler should keep animating, i.e. whether the
  // stall ramp still has frames left to play.
  function advanceDecay(now: number) {
    if (store.measuring || !store.stalledSince) return false;
    nowWall = now;
    engine?.wake();
    return now - store.stalledSince < STALL_DECAY_MS;
  }

  // The live region mirrors a value that updates every frame, so mid-phase
  // announcements are rate-limited to one per second — a screen reader would
  // otherwise never finish a sentence. Phase changes and any non-running update
  // jump the queue so the state is announced the instant it changes.
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
            : decayedBytesPerSec,
        scaleBytesPerSec: scale,
        latencyScaleMs,
        ticks: gaugeTicks,
        rtt: finalMetric?.kind === "latency" ? finalMetric.ms : store.liveRtt,
        completedKind,
      };
    });
    engine.attach(canvasEl!);
    decayPresentation = presentation.register(stageEl!, advanceDecay);

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
      decayPresentation.destroy();
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
      <div class="metric-wrap">
        <span class="gauge-value">{display.value}</span>
        {#if display.unit}<span class="gauge-unit">{display.unit}</span>{/if}
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
  /* Faceplate: the gauge panel is part of the instrument surface, not a floating
     card. It's flat and transparent; the gauge + latency panels are the
     engraved wells milled into it (--elev-inset), and the controls sit on the
     faceplate below them. */
  .gauge-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: 0;
    background: transparent;
    /* Container for .instrument's @container rule below — docked side panels
       can shrink this column independent of viewport width. Must live here,
       not on .instrument: a container query can only restyle descendants of
       the containment context, never the element itself. */
    container-type: inline-size;
    container-name: viz;
  }
  /* ---- The instrument grid ----
     One named-area grid places stage-head, gauge, Engage, and the optional
     latency panel, so the arrangement flips at a single breakpoint. The
     gauge+latency row is `minmax(220px, 1fr)` — a content-independent track,
     so the latency panel's content (which scrolls via its own overflow:auto)
     can never stretch the gauge's height. */
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
  /* No latency panel at all: drop its row entirely (rather than leaving an
     empty track) at any width. */
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
  /* The gauge well — the deepest recess on the faceplate, the signature. */
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
    /* Size query container so the hero number scales to the gauge's SMALLER
       dimension (cqmin) — the same dimension that sizes the ring. cqw alone
       overflowed in a wide-but-short well (ring sized by height, text by
       width); cqmin keeps the number proportional to the ring at any aspect. */
    container-type: size;
  }
  /* Engage's slot — RunButton centers itself (width:100%/max-width:320px/
     align-self:center); this slot only needs to be a flex row so that
     self-centering applies. */
  .engage-slot {
    grid-area: engage;
    display: flex;
    justify-content: center;
  }
  /* Latency profile — a matching engraved well; identical sizing to the gauge
     so the pair always reads as one balanced instrument. Its content scrolls
     within the shared height rather than forcing the row taller. */
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
    /* Keep the number clear of the gauge ring's sides; the inline padding also
       bounds how wide the value can get before the cqw sizing reins it in. */
    padding-inline: 9%;
    pointer-events: none;
  }
  /* The hero number — the one typographic moment. Space Grotesk (display),
     tabular figures so the live-updating value never shifts layout. Sized in
     cqw (relative to the gauge well) so large numbers shrink to fit a narrow
     gauge instead of overflowing; clamped so it stays legible and never huge. */
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
    /* No uppercase — unit symbols are case-significant (Mbit/s, kB/s, MiB/s). */
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
  /* Terminal-state headline (aborted / error): the WHAT, above the softer
     action line. Error is err-tinted; a user abort stays neutral (it isn't a
     failure) but reads at full text strength so the state is unmissable. */
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

  /* Stage-head block — Test Stages header + track. Placed by the instrument
     grid above (top on mobile, bottom on desktop — see .instrument). Capped
     to a comfortable measure and centered within its (possibly full-width,
     two-column-spanning) grid area so it never stretches absurdly wide on
     desktop. */
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

  /* Results slot — empty at idle, compact strip while a run is in progress,
     the full card grid once complete. The min-height reserve applies in EVERY
     state so the gauge above is the same size at page load, mid-run, at
     results, and back home. Wider than .stage-head/.controls-head above it
     (deliberately — this is the one element in the instrument allowed to
     outgrow that 600px measure): at up to 4 visible cards (download, upload,
     bidirectional, ping), a 600px cap forces an uneven 3-then-1 wrap; 760px
     comfortably fits all 4 in one row (4×~181px + 3×12px gap) while still
     reading fine at 1-3 cards, where the grid just stretches wider. */
  .results-slot {
    width: 100%;
    max-width: 760px;
    align-self: center;
    min-height: 108px;
  }
  /* Stacked/mobile: the document scrolls, so the anti-layout-shift reserve
     buys nothing and just reads as dead space between the controls and the
     chart. Collapse it — results push the chart down when they appear. */
  @media (max-width: 759px) {
    /* bp: stacked */
    .results-slot {
      min-height: 0;
    }
  }
</style>
