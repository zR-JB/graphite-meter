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

  // Gate the latency panel: hidden on a fresh idle load (gauge sits alone,
  // full-width), joins the row once a run starts and persists after completion
  // (leftover samples / result keep it shown so results stay readable).
  const showLatency = $derived(
    store.phase !== "idle" || store.latency.length > 0 || store.result != null,
  );

  // Total run ETA = warmup + each enabled stage's duration (read-only here;
  // duration itself is edited only in the Workbench, §14.2). Shown so the
  // newcomer knows roughly how long Engage will take.
  const etaMs = $derived(
    store.config.duration.warmupMs +
      (store.config.stages.latency ? store.config.duration.latencyMs : 0) +
      (store.config.stages.download ? store.config.duration.downloadMs : 0) +
      (store.config.stages.upload ? store.config.duration.uploadMs : 0),
  );

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
      intensity: store.liveMetric.value,
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
      <span class="eta" title="Estimated run time at the saved duration">
        ~{(etaMs / 1000).toFixed(0)}s
      </span>
    </div>
    <StageTrack />
    <EngageButton />
  </div>

</section>

<style>
  .reactor {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-card);
  }
  /* Peer row: gauge + latency panel are equal-importance flex siblings that
     wrap to stack when the row gets too narrow (responsive break-free). */
  .viz {
    display: flex;
    flex: 1 1 auto;
    flex-wrap: wrap;
    gap: 12px;
    min-height: 0;
  }
  .stage {
    position: relative;
    /* Equal-weight flex item, like the latency panel; floor keeps the gauge
       readable and the min-width forces a wrap (panel drops below) when tight. */
    flex: 1 1 320px;
    min-width: 280px;
    min-height: 210px;
    border-radius: var(--radius-md);
    background: var(--surface-inset);
    overflow: hidden;
  }
  /* Latency profile as a peer container on the same layer as the gauge — a
     matching inset panel that resizes with the gauge and wraps below it when
     the row is too narrow. */
  .latency-panel {
    flex: 1 1 320px;
    min-width: 260px;
    min-height: 210px;
    padding: 10px;
    border-radius: var(--radius-md);
    background: var(--surface-inset);
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
    pointer-events: none;
  }
  .reactor-metric {
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-size: clamp(30px, 4.4vw, 54px);
    font-weight: 700;
    letter-spacing: -0.04em;
    color: var(--text);
    line-height: 1;
  }
  .reactor-unit {
    margin-top: 4px;
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.04em;
    color: var(--text-soft);
    text-transform: uppercase;
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
  .controls {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 2px;
  }
  .controls-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }
  .controls-title {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-soft);
  }
  .eta {
    font-family: var(--font-mono);
    font-size: 12px;
    font-weight: 700;
    color: var(--text-muted);
    letter-spacing: 0;
  }
</style>
