<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { tooltip } from "../actions/tooltip";
  import {
    presentation,
    type PresentationHandle,
  } from "../canvas/presentation";
  import {
    canvasPixelRatio,
    watchCanvasPixelRatio,
  } from "../canvas/canvasResolution";

  // CSS size of the sparkline. Mirrors the canvas width/height attributes and
  // the .spark rule, so the backing store scales by the device ratio.
  const CSS_WIDTH = 36;
  const CSS_HEIGHT = 16;
  const FALLBACK_COLOR = "#888";

  let canvasEl = $state<HTMLCanvasElement>();
  let canvasPresentation: PresentationHandle;

  const spark = $derived(
    store.pulseLatency
      .slice(-16)
      .flatMap((bucket) =>
        bucket.medianRttMs == null ? [] : [bucket.medianRttMs],
      ),
  );

  let sparkColor = FALLBACK_COLOR;
  function resolveColor() {
    sparkColor =
      getComputedStyle(document.documentElement)
        .getPropertyValue("--text-soft")
        .trim() || FALLBACK_COLOR;
  }

  // A one-shot repaint, re-armed by invalidate().
  function draw(): boolean {
    const canvas = canvasEl;
    if (!canvas) return false;
    const ctx = canvas.getContext("2d");
    if (!ctx) return false;
    const dpr = canvasPixelRatio();
    const deviceWidth = Math.round(CSS_WIDTH * dpr);
    const deviceHeight = Math.round(CSS_HEIGHT * dpr);
    if (canvas.width !== deviceWidth || canvas.height !== deviceHeight) {
      canvas.width = deviceWidth;
      canvas.height = deviceHeight;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, CSS_WIDTH, CSS_HEIGHT);

    const samples = spark;
    if (samples.length < 2) return false;
    const min = Math.min(...samples);
    const max = Math.max(...samples);
    const range = max - min || 1;
    ctx.strokeStyle = sparkColor;
    ctx.lineWidth = 1;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.beginPath();
    samples.forEach((rttMs, i) => {
      const x = (i / (samples.length - 1)) * (CSS_WIDTH - 2) + 1;
      const y = CSS_HEIGHT - 1 - ((rttMs - min) / range) * (CSS_HEIGHT - 2);
      if (i) ctx.lineTo(x, y);
      else ctx.moveTo(x, y);
    });
    ctx.stroke();
    return false;
  }

  $effect(() => {
    void spark;
    canvasPresentation?.invalidate();
  });

  onMount(() => {
    resolveColor();
    canvasPresentation = presentation.register(canvasEl!, draw);
    const themeObserver = new MutationObserver(() => {
      resolveColor();
      canvasPresentation.invalidate();
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    const stopWatchingPixelRatio = watchCanvasPixelRatio(() =>
      canvasPresentation.invalidate(),
    );
    return () => {
      canvasPresentation.destroy();
      themeObserver.disconnect();
      stopWatchingPixelRatio();
    };
  });
</script>

<div
  class="pulse"
  role="status"
  aria-label={`Connection: ${store.effectiveConnectivity}`}
  use:tooltip={`Connection: ${store.effectiveConnectivity}`}
>
  <span class="dot" data-state={store.effectiveConnectivity}></span>
  <canvas
    bind:this={canvasEl}
    class="spark"
    width="36"
    height="16"
    aria-hidden="true"
  ></canvas>
</div>

<style>
  .pulse {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: 0 6px;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: var(--r-full);
    flex: 0 0 auto;
  }
  .spark {
    width: 36px;
    height: 16px;
    display: block;
    opacity: 0.85;
  }

  /* State tones. */
  .dot[data-state="connected"] {
    background: var(--ok);
    box-shadow: 0 0 0 4px var(--ok-soft);
  }
  .dot[data-state="degraded"] {
    background: var(--warn);
    box-shadow: 0 0 0 4px var(--warn-soft);
  }
  .dot[data-state="unstable"] {
    background: var(--err);
    box-shadow: 0 0 0 4px var(--err-soft);
  }
  .dot[data-state="offline"] {
    background: transparent;
    border: 1.5px solid var(--text-soft);
  }
</style>
