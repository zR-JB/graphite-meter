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
  import PhaseRail from "./PhaseRail.svelte";
  import StageChips from "./StageChips.svelte";
  import EngageButton from "./EngageButton.svelte";
  import { fmtSpeed, fmtMs } from "../format";

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
      a11y = `${display.value} ${display.unit}, phase ${store.phase}`;
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
  <div class="stage">
    <canvas bind:this={canvasEl} class="canvas" aria-hidden="true"></canvas>
    <div class="metric-wrap">
      <span class="reactor-metric">{display.value}</span>
      {#if display.unit}<span class="reactor-unit">{display.unit}</span>{/if}
    </div>
    <output class="sr-only" aria-live="polite">{a11y}</output>
  </div>
  <PhaseRail />

  <!-- Hero controls — gauge + number + stage chips + Engage read as one
       instrument (§14.2). Duration is Workbench-only; the run uses the
       saved duration and shows just its ETA here. -->
  <div class="controls">
    <div class="controls-head">
      <span class="controls-title">Test stages</span>
      <span class="eta" title="Estimated run time at the saved duration">
        ~{(etaMs / 1000).toFixed(0)}s
      </span>
    </div>
    <StageChips />
    <EngageButton />
  </div>
</section>

<style>
  .reactor {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-card);
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 230px;
    border-radius: var(--radius-md);
    background: var(--surface-inset);
    overflow: hidden;
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

  /* Hero controls block — stage chips + the master Engage action, sitting
     directly under the gauge so the three read as one instrument. */
  .controls {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding-top: 4px;
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
