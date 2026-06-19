<script lang="ts">
  /* ============================================================
   * <ConnectivityPulse> — ambient awareness, topbar (§3.4)
   * A 9px state dot (pulse keyed to effectiveConnectivity) + a
   * 36px micro-sparkline of the last 16 RTT samples. Decorative
   * canvas (aria-hidden); the wrapper carries role=status so the
   * connection state is announced + tooltip-discoverable.
   * ============================================================ */
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { tooltip } from "../actions/tooltip";

  let canvasEl = $state<HTMLCanvasElement>();

  // Last 16 RTT samples drive the sparkline.
  const spark = $derived(store.latency.slice(-16).map((s) => s.rttMs));

  function draw() {
    const c = canvasEl;
    if (!c) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const w = 36;
    const h = 16;
    c.width = Math.round(w * dpr);
    c.height = Math.round(h * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const vals = spark;
    if (vals.length < 2) return;
    const min = Math.min(...vals);
    const max = Math.max(...vals);
    const range = max - min || 1;
    const col =
      getComputedStyle(document.documentElement).getPropertyValue("--text-soft").trim() ||
      "#888";
    ctx.strokeStyle = col;
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
  }

  // Redraw when samples change (tracking `spark`).
  $effect(() => {
    spark;
    draw();
  });

  onMount(() => {
    draw();
    // Theme switch recolors the sparkline.
    const mo = new MutationObserver(draw);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    return () => mo.disconnect();
  });
</script>

<div
  class="pulse"
  role="status"
  aria-label={`Connection: ${store.effectiveConnectivity}`}
  use:tooltip={`Connection: ${store.effectiveConnectivity}`}
>
  <span class="dot" data-state={store.effectiveConnectivity}></span>
  <canvas bind:this={canvasEl} class="spark" width="36" height="16" aria-hidden="true"></canvas>
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

  /* State tones + pulse cadence (§3.4). */
  .dot[data-state="connected"] {
    background: var(--ok);
    box-shadow: 0 0 0 4px var(--ok-soft);
    animation: pulse 2s var(--ease-out) infinite;
  }
  .dot[data-state="degraded"] {
    background: var(--warn);
    box-shadow: 0 0 0 4px var(--warn-soft);
    animation: pulse 1s var(--ease-out) infinite;
  }
  .dot[data-state="unstable"] {
    background: var(--err);
    box-shadow: 0 0 0 4px var(--err-soft);
    animation: pulse 0.6s var(--ease-out) infinite;
  }
  .dot[data-state="offline"] {
    background: transparent;
    border: 1.5px solid var(--text-soft);
    animation: blink 2.4s steps(1, end) infinite;
  }

  @keyframes pulse {
    0% {
      transform: scale(0.92);
      filter: brightness(1);
    }
    50% {
      transform: scale(1.08);
      filter: brightness(1.25);
    }
    100% {
      transform: scale(0.92);
      filter: brightness(1);
    }
  }
  @keyframes blink {
    0%,
    60% {
      opacity: 1;
    }
    80%,
    100% {
      opacity: 0.35;
    }
  }

  /* Reduced motion: hold steady, no pulse/blink. */
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none !important;
    }
  }
</style>
