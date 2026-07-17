<script lang="ts">
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { tooltip } from "../actions/tooltip";
  import {
    presentation,
    type PresentationHandle,
  } from "../canvas/presentation";

  let canvasEl = $state<HTMLCanvasElement>();
  let canvasPresentation: PresentationHandle;

  const spark = $derived(store.pulseLatency.slice(-16).map((s) => s.rttMs));

  let sparkColor = "#888";
  function resolveColor() {
    sparkColor =
      getComputedStyle(document.documentElement)
        .getPropertyValue("--text-soft")
        .trim() || "#888";
  }

  function draw(): boolean {
    const c = canvasEl;
    if (!c) return false;
    const ctx = c.getContext("2d");
    if (!ctx) return false;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const w = 36;
    const h = 16;
    const width = Math.round(w * dpr);
    const height = Math.round(h * dpr);
    if (c.width !== width || c.height !== height) {
      c.width = width;
      c.height = height;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const vals = spark;
    if (vals.length < 2) return false;
    const min = Math.min(...vals);
    const max = Math.max(...vals);
    const range = max - min || 1;
    ctx.strokeStyle = sparkColor;
    ctx.lineWidth = 1;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.beginPath();
    vals.forEach((v, i) => {
      const x = (i / (vals.length - 1)) * (w - 2) + 1;
      const y = h - 1 - ((v - min) / range) * (h - 2);
      if (i) ctx.lineTo(x, y);
      else ctx.moveTo(x, y);
    });
    ctx.stroke();
    return false;
  }

  $effect(() => {
    spark;
    canvasPresentation?.invalidate();
  });

  onMount(() => {
    resolveColor();
    canvasPresentation = presentation.register(canvasEl!, draw);
    const mo = new MutationObserver(() => {
      resolveColor();
      canvasPresentation.invalidate();
    });
    mo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => {
      canvasPresentation.destroy();
      mo.disconnect();
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
    gap: 8px;
    padding: 0 6px;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 999px;
    flex: 0 0 auto;
  }
  .spark {
    width: 36px;
    height: 16px;
    display: block;
    opacity: 0.85;
  }

  /* State tones + pulse cadence. */
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
