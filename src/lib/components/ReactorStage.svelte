<script lang="ts">
  /* ============================================================
   * <ReactorStage> — the signature visualization (§3.1)
   * Thin wrapper: instantiates ReactorEngine on mount, feeds it a
   * pull-callback, and reacts to theme/resize. The live primary
   * metric is plain DOM (zero layout shift via tabular-nums +
   * fmtSpeed banding); the canvas is decorative (aria-hidden).
   * ============================================================ */
  import { onMount } from "svelte";
  import { console as store } from "../state/console.svelte";
  import { GaugeEngine } from "../canvas/GaugeEngine";
  import StageTrack from "./StageTrack.svelte";
  import EngageButton from "./EngageButton.svelte";
  import LatencyProfile from "./LatencyProfile.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { tooltip } from "../actions/tooltip";
  import { preStageWarmupMs } from "../runner/contract";

  // Gate the latency panel: hidden on a fresh idle load (gauge sits alone,
  // full-width), joins the row once a run starts and persists after completion
  // (leftover samples / result keep it shown so results stay readable).
  const showLatency = $derived(
    store.phase !== "idle" || store.latency.length > 0 || store.result != null,
  );

  // Total run ETA = warmup + each enabled stage's duration (read-only here;
  // duration itself is edited only in the Workbench, §14.2). Shown so the
  // newcomer knows roughly how long Engage will take.
  const etaMs = $derived.by(() => {
    const d = store.config.duration;
    const pre = preStageWarmupMs(d.warmupMs);
    return (
      d.warmupMs +
      (store.config.stages.latency ? d.latencyMs : 0) +
      (store.config.stages.download ? pre + d.downloadMs : 0) +
      (store.config.stages.upload ? pre + d.uploadMs : 0)
    );
  });

  let canvasEl = $state<HTMLCanvasElement>();
  let engine: GaugeEngine;

  // The single big number, per phase (§3.1 behavior table).
  const display = $derived.by(() => {
    const p = store.phase;
    if (p === "latency") return { value: fmtMs(store.liveRtt), unit: "ms" };
    if (p === "idle" || p === "error" || p === "aborted") return { value: "—", unit: "" };
    if (p === "complete" && store.result?.download) {
      return {
        value: fmtSpeed(store.toUnit(store.result.download.meanBps)),
        unit: store.unitLabel,
      };
    }
    return { value: fmtSpeed(store.liveMetric.value), unit: store.liveMetric.unit };
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
        return store.errorMsg ?? "Something went wrong — press Engage to retry";
      default:
        return "";
    }
  });

  // Screen-reader mirror, throttled to 1Hz + phase changes (§7).
  let a11y = $state("");

  onMount(() => {
    engine = new GaugeEngine(() => ({
      phase: store.phase,
      valueBps: store.throughput.at(-1)?.bps ?? 0,
      scaleBps: store.displayScaleBps,
      // Five quarter labels (0 … full scale) in the active display unit.
      ticks: [0, 0.25, 0.5, 0.75, 1].map((f) =>
        fmtSpeed(store.toUnit(store.displayScaleBps * f)),
      ),
      rtt: store.liveRtt,
      pingCount: store.latency.length,
    }));
    engine.attach(canvasEl!);
    engine.start();

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
