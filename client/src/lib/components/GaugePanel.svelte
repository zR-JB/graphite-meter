<script lang="ts">
  /* ============================================================
   * <GaugePanel> — the signature visualization (§3.1)
   * Thin wrapper: instantiates GaugeEngine on mount, feeds it a
   * pull-callback, and reacts to theme/resize. The live primary
   * metric is plain DOM (zero layout shift via tabular-nums +
   * fmtSpeed banding); the canvas is decorative (aria-hidden).
   * ============================================================ */
  import { onMount, untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import { GaugeEngine } from "../canvas/GaugeEngine";
  import StageTrack from "./StageTrack.svelte";
  import RunButton from "./RunButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import ResultCards from "./ResultCards.svelte";
  import { fmtSpeed, fmtMs, reasonLabel } from "../format";
  import { tooltip } from "../actions/tooltip";

  // Gate the latency panel purely on whether latency is measured at all — the
  // enabled-stage state is the single source of truth. So a wordmark "home"
  // reset and a page reload land on the same view: panel shown when latency is
  // enabled (even on the blank idle gauge), hidden when it isn't.
  const showLatency = $derived(store.latencyEnabled);

  // ---- Instrument / results tri-state ----
  // idle/running shows just the dial; once a run is underway a compact
  // one-line-per-stage strip appears; on completion it swaps to the full
  // result-card grid. The slot below the gauge is ALWAYS mounted (reserved at
  // its compact-strip height outside the final state) so the gauge height is
  // identical at page load, mid-run, and after a return to the idle view.
  const resultsView = $derived.by<"none" | "partial" | "final">(() => {
    if (store.phase === "complete") return "final";
    if (store.phase === "idle") return "none";
    return "partial";
  });

  // Total run ETA (read-only here; duration is edited only in Settings, §14.2).
  // Backed by the shared scheduler via store.totalEtaMs so it can't drift from
  // the real timeline (and counts bidirectional when on). Shown so a newcomer
  // knows roughly how long Engage will take.
  const etaMs = $derived(store.totalEtaMs);

  let canvasEl = $state<HTMLCanvasElement>();
  let engine: GaugeEngine;

  // ── Grind-to-zero on a stall (presentation only — principle 2) ──
  // While the run is stalled (no real samples arriving) the gauge needle + the
  // big live number EASE to 0 over ~800ms instead of snapping, then snap back
  // the instant a real sample resumes. This is a pure DRAW-TIME effect computed
  // from real state (store.stalledSince + the last real sample) — it stores
  // nothing, emits nothing, and pushes no sample into any buffer. Showing ~0
  // during dead air is in fact truthful (no bytes are arriving); we just ease
  // the transition. Needs a per-frame wall clock, so a tiny rAF loop bumps
  // `nowWall`; the GaugeEngine's own EMA follower carries the needle, while the
  // DOM number multiplies the last real bytes/sec by the same decay factor.
  const STALL_DECAY_MS = 800;
  let nowWall = $state(performance.now());
  const stallDecay = $derived.by(() => {
    if (store.measuring || !store.stalledSince) return 1;
    const since = nowWall - store.stalledSince;
    return Math.min(1, Math.max(0, 1 - since / STALL_DECAY_MS));
  });
  // The live transfer rate, eased toward 0 while stalled. The store's samples
  // are ALREADY de-aliased at the single smoothing point in RunnerCore, so both
  // the big number and the needle (via the engine's 60Hz interpolation EMA) read
  // from that one smoothed source — no extra UI-layer smoothing here.
  const decayedBytesPerSec = $derived(store.liveTransferBytesPerSec * stallDecay);

  // Nice-ceiling ladder (ms) for the latency dial. 1-2-5 steps so the five
  // quarter labels (0 … scale) always land on clean values (5/10/25/50…).
  const LATENCY_SCALE_LADDER = [20, 40, 100, 200, 400, 1000, 2000, 4000];

  // A fixed, linear ms scale for the latency-phase dial: round the running peak
  // RTT (incl. the pre-test ping) up to the next nice ceiling, so the needle
  // position reads as a real RTT and the tick labels stay round.
  const latencyScaleMs = $derived.by(() => {
    let peak = store.infra?.preTestPingMs ?? 0;
    for (const s of store.latency) if (!s.lost && s.rttMs > peak) peak = s.rttMs;
    const target = peak * 1.1; // a touch of headroom so the peak isn't pegged
    return LATENCY_SCALE_LADDER.find((s) => s >= target) ?? LATENCY_SCALE_LADDER.at(-1)!;
  });

  // Quarter tick labels for the dial, memoized so the engine's 60 Hz pull
  // doesn't re-format ten strings per frame — this only recomputes when the
  // phase, scale, or display unit actually changes.
  const msTicksActive = $derived(
    store.phase === "latency" ||
      (store.phase === "complete" && store.finalMetric?.kind === "latency"),
  );
  const gaugeTicks = $derived.by(() => {
    if (msTicksActive) return [0, 0.25, 0.5, 0.75, 1].map((f) => fmtMs(latencyScaleMs * f));
    const scale = store.displayScaleBytesPerSec;
    return [0, 0.25, 0.5, 0.75, 1].map((f) => fmtSpeed(store.toUnit(scale * f)));
  });

  // The single big number, per phase (§3.1 behavior table).
  const display = $derived.by(() => {
    const p = store.phase;
    if (p === "latency") return { value: fmtMs(store.liveRtt), unit: "ms" };
    // Warmup has no meaningful rate yet — show the same neutral dash as the
    // idle/error/aborted states rather than a misleading 0 bit/s.
    if (p === "idle" || p === "error" || p === "aborted" || p === "warmup")
      return { value: "—", unit: "" };
    if (p === "complete") {
      // Phase-agnostic headline: download if it ran, else upload, else latency
      // — never assume download exists.
      const fm = store.finalMetric;
      if (fm?.kind === "speed")
        return { value: fmtSpeed(store.toUnit(fm.bytesPerSec)), unit: store.unitLabel };
      if (fm?.kind === "latency") return { value: fmtMs(fm.ms), unit: "ms" };
      return { value: "—", unit: "" };
    }
    // Live download/upload/bidirectional speed (already de-aliased upstream).
    // While stalled it eases to 0 over ~800ms (presentation only — principle 2);
    // snaps back on resume.
    return { value: fmtSpeed(store.toUnit(decayedBytesPerSec)), unit: store.unitLabel };
  });

  // Skipped transfer stages — the gauge explains why throughput is missing.
  const STAGE_NAME: Record<string, string> = {
    latency: "Latency",
    download: "Download",
    upload: "Upload",
    bidirectional: "Bidirectional",
  };
  const failNotes = $derived(
    store.transferFailures.map((f) => `${STAGE_NAME[f.stage]} skipped — ${f.message}`),
  );

  // Guided idle / empty + transient states (§14.3) — never a dead, bare dash.
  // Shown as soft copy beneath the big metric so a newcomer always knows what
  // to do (idle) or what is happening (warmup probing).
  const hint = $derived.by(() => {
    switch (store.phase) {
      case "idle":
        return "Press Engage to start your speed test";
      case "warmup":
        return "Checking your connection…";
      default:
        return "";
    }
  });

  // Terminal aborted/error states get a distinct two-line treatment: a
  // headline naming WHAT happened (friendly reason copy via reasonLabel —
  // never the backend's raw engineering message) and an action line that
  // matches the button's actual label ("Run Again", not the old "Engage").
  const status = $derived.by(() => {
    switch (store.phase) {
      case "aborted":
        return { tone: "aborted", headline: "Test aborted", action: "Press Run Again to restart" };
      case "error":
        return {
          tone: "error",
          headline: store.error ? reasonLabel(store.error.reason) : "Something went wrong",
          action: "Press Run Again to retry",
        };
      default:
        return null;
    }
  });

  // One string for the screen-reader mirror: the status (when terminal) or
  // the guided hint (idle/warmup); empty mid-run (the live value speaks).
  const statusText = $derived(status ? `${status.headline} — ${status.action}` : hint);

  // Wake the (self-parking) gauge loop whenever the live state it draws from
  // changes. During a run the loop sustains itself; this re-arms it on the
  // idle→engage transition and any discrete change while parked.
  $effect(() => {
    // Track the fields the gauge pulls so a change re-runs this effect.
    void store.phase;
    void store.throughput.length;
    void store.latency.length;
    void store.liveRtt;
    void store.displayScaleBytesPerSec;
    void store.measuring; // re-arm on a stall/resume edge so the decay animates
    // Re-arm on a unit/base toggle so the tick labels re-format after the run
    // finishes and the loop has parked (the ticks read store.toUnit live).
    void store.unitBase;
    void store.unitKind;
    // unitLabel changes whenever the shared prefix index (k/M/G/T) moves —
    // that can happen from the raw peak alone, independent of
    // displayScaleBytesPerSec (which tracks the DWELL-FILTERED sustained
    // peak, a different signal). Without this, a prefix change while the
    // loop is parked would leave the canvas-drawn tick labels stale.
    void store.unitLabel;
    engine?.wake();
  });

  // Per-frame wall clock for the grind-to-zero decay. It only needs to tick
  // while a stall is easing the value down (and one frame after resume to snap
  // back); a self-parking rAF keeps it idle otherwise. Bumping `nowWall`
  // recomputes `stallDecay`/`decayedBytesPerSec`, which the DOM number reads and
  // which feeds the gauge engine's needle EMA.
  let decayRaf = 0;
  $effect(() => {
    const easing = store.isRunning && !store.measuring;
    if (!easing) {
      if (decayRaf) cancelAnimationFrame(decayRaf);
      decayRaf = 0;
      return;
    }
    const loop = () => {
      nowWall = performance.now();
      engine?.wake(); // keep the needle's EMA following the decayed value
      decayRaf = requestAnimationFrame(loop);
    };
    decayRaf = requestAnimationFrame(loop);
    return () => {
      if (decayRaf) cancelAnimationFrame(decayRaf);
      decayRaf = 0;
    };
  });

  // Screen-reader mirror (§7): the live value is refreshed on a 1Hz tick (below)
  // so a 16Hz number doesn't flood the live region, but phase/hint changes — the
  // semantic events — are announced immediately via this effect.
  let a11y = $state("");
  $effect(() => {
    const s = statusText; // track phase-driven copy + phase changes; not the live value
    void store.phase;
    a11y = s ? s : untrack(() => `${display.value} ${display.unit}, phase ${store.phase}`);
  });

  onMount(() => {
    engine = new GaugeEngine(() => {
      const p = store.phase;
      const fm = store.finalMetric;
      const scale = store.displayScaleBytesPerSec;
      // At complete, resolve the dial + its tick scale to the primary result
      // stage (download→upload→latency) so a run without download still reads
      // right (needle and labels match, instead of an RTT needle under speed
      // labels). -1 elsewhere → the per-phase live logic drives the dial.
      let resolvedFraction = -1;
      if (p === "complete" && fm) {
        if (fm.kind === "speed") {
          resolvedFraction = scale > 0 ? Math.min(1, Math.max(0, fm.bytesPerSec / scale)) : 0;
        } else {
          resolvedFraction = latencyScaleMs > 0 ? Math.min(1, Math.max(0, fm.ms / latencyScaleMs)) : 0;
        }
      }
      return {
        phase: p,
        // Stall-decayed (presentation only — principle 2): the needle eases to
        // 0 during dead air via the engine's EMA, snaps back on resume.
        valueBytesPerSec: decayedBytesPerSec,
        scaleBytesPerSec: scale,
        latencyScaleMs,
        resolvedFraction,
        // Five quarter labels (0 … full scale) — memoized above: ms during the
        // latency phase or a latency-resolved end state, else throughput.
        ticks: gaugeTicks,
        rtt: store.liveRtt,
        pingCount: store.latency.length,
      };
    });
    engine.attach(canvasEl!);
    engine.start();

    // invalidateTheme repaints synchronously, so theme/resize need no wake().
    const mo = new MutationObserver(() => engine.invalidateTheme());
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

    const ro = new ResizeObserver(() => engine.invalidateTheme());
    ro.observe(canvasEl!);

    const tick = setInterval(() => {
      // Prefer the guided copy when there's no live number (idle/warmup/error),
      // otherwise announce the measured metric (factual only — no verdict §14.3).
      a11y = statusText
        ? statusText
        : `${display.value} ${display.unit}, phase ${store.phase}`;
    }, 1000);

    return () => {
      engine.destroy();
      mo.disconnect();
      ro.disconnect();
      clearInterval(tick);
    };
  });
</script>

<section class="gauge-panel">
  <!-- Hero instrument — gauge, Engage, the stage selector, and (optionally)
       the latency profile are all placed via ONE named-area CSS Grid inside
       a single container-query context (§14.2 update). Their arrangement
       flips ATOMICALLY at one breakpoint (see .instrument's @container rule
       below) instead of several independent thresholds, and the shared
       gauge+latency row gets an explicit, content-independent track size so
       toggling the latency panel on/off can never change the gauge's height:
         Desktop (wide): gauge+latency side by side, Engage below them,
           Test Stages at the very bottom (keeps the space below the
           gauge from looking empty).
         Mobile (narrow/stacked): Test Stages at the very top, then gauge,
           then Engage, then the latency panel below. -->
  <div class="instrument">
    <div class="stage-head">
      <div class="controls-head">
        <span class="controls-title">Test stages</span>
        <span class="eta" use:tooltip={"Estimated run time at the saved duration"}>
          ~{(etaMs / 1000).toFixed(0)}s
        </span>
      </div>
      <StageTrack />
    </div>

    <div class="stage">
      <canvas bind:this={canvasEl} class="canvas" aria-hidden="true"></canvas>
      <div class="metric-wrap">
        <span class="gauge-value">{display.value}</span>
        {#if display.unit}<span class="gauge-unit">{display.unit}</span>{/if}
        {#if hint || status || failNotes.length}
          <div class="gauge-notes">
            {#each failNotes as note (note)}<span class="gauge-fail">{note}</span>{/each}
            {#if status}
              <span class="gauge-status" class:error={status.tone === "error"}>
                {status.headline}
              </span>
              <span class="gauge-hint">{status.action}</span>
            {:else if hint}<span class="gauge-hint">{hint}</span>{/if}
          </div>
        {/if}
      </div>
      <output class="sr-only" aria-live="polite">{a11y}</output>
    </div>

    <div class="engage-slot"><RunButton /></div>

    {#if showLatency}
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
  /* Notes zone at the dial's foot: guided idle/transient copy (§14.3) and
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
     results, and back home. */
  .results-slot {
    width: 100%;
    max-width: 600px;
    align-self: center;
    min-height: 108px;
  }
  /* Stacked/mobile: the document scrolls, so the anti-layout-shift reserve
     buys nothing and just reads as dead space between the controls and the
     chart. Collapse it — results push the chart down when they appear. */
  @media (max-width: 759px) { /* bp: stacked */
    .results-slot {
      min-height: 0;
    }
  }
</style>
