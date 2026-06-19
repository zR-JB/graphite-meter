<script lang="ts">
  /* ============================================================
   * <ReactorStage> — the signature visualization (§3.1)
   * Thin wrapper: instantiates ReactorEngine on mount, feeds it a
   * pull-callback, and reacts to theme/resize. The live primary
   * metric is plain DOM (zero layout shift via tabular-nums +
   * fmtSpeed banding); the canvas is decorative (aria-hidden).
   * ============================================================ */
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { GaugeEngine } from "../canvas/GaugeEngine";
  import StageTrack from "./StageTrack.svelte";
  import EngageButton from "./EngageButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { tooltip } from "../actions/tooltip";

  // Gate the latency panel purely on whether latency is measured at all — the
  // enabled-stage state is the single source of truth. So a wordmark "home"
  // reset and a page reload land on the same view: panel shown when latency is
  // enabled (even on the blank idle gauge), hidden when it isn't.
  const showLatency = $derived(store.latencyEnabled);

  // Total run ETA = warmup + each enabled stage's duration (read-only here;
  // duration itself is edited only in the Workbench, §14.2). Shown so the
  // newcomer knows roughly how long Engage will take.
  const etaMs = $derived.by(() => {
    const d = store.config.duration;
    const st = store.config.stages;
    // Mirror the runner timeline (dummy.start): every enabled stage contributes
    // its own warmup plus its measurement. No global warmup, no merging.
    const w = d.warmupMs > 0 ? d.warmupMs : 0;
    let total = 0;
    if (st.latency && d.latencyMs > 0) total += w + d.latencyMs;
    if (st.download && d.downloadMs > 0) total += w + d.downloadMs;
    if (st.upload && d.uploadMs > 0) total += w + d.uploadMs;
    return total;
  });

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
  // The last REAL throughput value, eased toward 0 while stalled. Read-only of
  // the buffer's tail — never mutated.
  const decayedBytesPerSec = $derived((store.throughput.at(-1)?.bytesPerSec ?? 0) * stallDecay);

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
    // Live download/upload/bidirectional speed. While stalled the value eases
    // to 0 over ~800ms (presentation only — principle 2); snaps back on resume.
    return { value: fmtSpeed(store.toUnit(decayedBytesPerSec)), unit: store.unitLabel };
  });

  // Guided idle / empty + transient states (§14.3) — never a dead, bare dash.
  // Shown as soft copy beneath the big metric so a newcomer always knows what
  // to do (idle) or what is happening (warmup probing / error / aborted).
  const hint = $derived.by(() => {
    switch (store.phase) {
      case "idle":
        return "Press Engage to start your speed test";
      case "warmup":
        return "Checking your connection…";
      case "aborted":
        return "Test stopped — press Engage to try again";
      case "error":
        return store.error
          ? `${store.error.message} — press Engage to retry`
          : "Something went wrong — press Engage to retry";
      default:
        return "";
    }
  });

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
    engine?.wake();
  });

  // Per-frame wall clock for the grind-to-zero decay. It only needs to tick
  // while a stall is easing the value down (and one frame after resume to snap
  // back); a self-parking rAF keeps it idle otherwise. Bumping `nowWall`
  // recomputes `stallDecay`/`decayedBytesPerSec`, which the DOM number reads
  // and which (via the getter) feeds the gauge's EMA.
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

  // Screen-reader mirror, throttled to 1Hz + phase changes (§7).
  let a11y = $state("");

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
      let msTicks = p === "latency";
      if (p === "complete" && fm) {
        if (fm.kind === "speed") {
          resolvedFraction = scale > 0 ? Math.min(1, Math.max(0, fm.bytesPerSec / scale)) : 0;
        } else {
          resolvedFraction = latencyScaleMs > 0 ? Math.min(1, Math.max(0, fm.ms / latencyScaleMs)) : 0;
          msTicks = true;
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
        // Five quarter labels (0 … full scale): ms during the latency phase or
        // a latency-resolved end state, otherwise throughput in the active unit.
        ticks: msTicks
          ? [0, 0.25, 0.5, 0.75, 1].map((f) => fmtMs(latencyScaleMs * f))
          : [0, 0.25, 0.5, 0.75, 1].map((f) => fmtSpeed(store.toUnit(scale * f))),
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
      // Prefer the guided hint when there's no live number (idle/warmup/error),
      // otherwise announce the measured metric (factual only — no verdict §14.3).
      a11y = hint
        ? hint
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

<section class="reactor">
  <!-- Gauge and latency profile are peer containers on the same layer (§14.2):
       a wrapping flex row so they resize together on wide layouts and the
       latency panel breaks free below the gauge when space gets tight. The
       latency panel only joins the row once a run has produced data. -->
  <div class="viz">
    <div class="stage">
      <canvas bind:this={canvasEl} class="canvas" aria-hidden="true"></canvas>
      <div class="metric-wrap">
        <span class="reactor-metric">{display.value}</span>
        {#if display.unit}<span class="reactor-unit">{display.unit}</span>{/if}
        {#if hint}<span class="reactor-hint">{hint}</span>{/if}
      </div>
      <output class="sr-only" aria-live="polite">{a11y}</output>
    </div>

    {#if showLatency}
      <div class="latency-panel">
        <LatencyProfile bare />
      </div>
    {/if}
  </div>

  <!-- Hero controls — gauge + number + combined stage track + Engage read as
       one instrument (§14.2). The StageTrack is BOTH the stage selector and the
       live phase-progress indicator (no standalone warmup/✓ segments). Duration
       is Workbench-only; the run uses the saved duration and shows its ETA. -->
  <div class="controls">
    <div class="controls-head">
      <span class="controls-title">Test stages</span>
      <span class="eta" use:tooltip={"Estimated run time at the saved duration"}>
        ~{(etaMs / 1000).toFixed(0)}s
      </span>
    </div>
    <StageTrack />
    <EngageButton />
  </div>

</section>

<style>
  /* Faceplate: the reactor is part of the instrument surface, not a floating
     card. It's flat and transparent; the gauge + latency panels are the
     engraved wells milled into it (--elev-inset), and the controls sit on the
     faceplate below them. */
  .reactor {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: 0;
    background: transparent;
  }
  /* Peer row: gauge + latency panel are equal-importance siblings. They share
     an identical flex basis + min-width + min-height and align-items:stretch,
     so side-by-side they are always exactly equal in both dimensions; when the
     row can't hold both at their floor they wrap and stack cleanly. min-width
     is a modest wrap-trigger (well under any mobile width) so a single panel
     can never overflow horizontally. */
  .viz {
    display: flex;
    flex: 1 1 auto;
    flex-wrap: wrap;
    align-items: stretch;
    gap: var(--space-3);
    min-height: 0;
  }
  /* The gauge well — the deepest recess on the faceplate, the signature. */
  .stage {
    position: relative;
    flex: 1 1 300px;
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
  /* Latency profile — a matching engraved well; identical sizing to the gauge
     so the pair always reads as one balanced instrument. Its content scrolls
     within the shared height rather than forcing the row taller. */
  .latency-panel {
    flex: 1 1 300px;
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
  .reactor-metric {
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
  .reactor-unit {
    margin-top: var(--space-1);
    font-family: var(--font-mono);
    font-size: var(--type-sm);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--text-soft);
    /* No uppercase — unit symbols are case-significant (Mbit/s, kB/s, MiB/s). */
  }
  /* Guided idle / empty-state copy (§14.3) — replaces the dead bare dash so the
     gauge always invites action or explains what's happening. Sits centered
     beneath the big metric; doesn't affect the metric's zero-shift baseline. */
  .reactor-hint {
    position: absolute;
    bottom: 18px;
    left: 50%;
    transform: translateX(-50%);
    max-width: 86%;
    text-align: center;
    font-size: 12.5px;
    font-weight: 600;
    line-height: 1.35;
    color: var(--text-muted);
  }

  /* Hero controls block — stage track + the master Engage action, sitting
     directly under the gauge so the three read as one instrument. */
  /* The controls group (stage header + track + Engage) reads as one
     instrument cluster. Capped to a comfortable measure and centered so the
     stage track spans the full width only when the viewport is genuinely
     narrow (mobile) and never stretches absurdly wide on desktop. */
  .controls {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding-top: 0;
    width: 100%;
    max-width: 600px;
    align-self: center;
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
</style>
