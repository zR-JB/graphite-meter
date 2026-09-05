<script lang="ts">
  import { store } from "../state/store.svelte";
  import { tooltip } from "../actions/tooltip";

  const spark = $derived(
    store.pulseLatency
      .slice(-16)
      .flatMap((bucket) =>
        bucket.medianRttMs == null ? [] : [bucket.medianRttMs],
      ),
  );

  const points = $derived.by(() => {
    if (spark.length < 2) return "";
    const min = Math.min(...spark);
    const range = Math.max(...spark) - min || 1;
    return spark
      .map(
        (rttMs, i) =>
          `${1 + (i / (spark.length - 1)) * 34},${15 - ((rttMs - min) / range) * 14}`,
      )
      .join(" ");
  });
</script>

<div
  class="pulse"
  role="status"
  aria-label={`Connection: ${store.effectiveConnectivity}`}
  use:tooltip={`Connection: ${store.effectiveConnectivity}`}
>
  <span class="dot" data-state={store.effectiveConnectivity}></span>
  <svg class="spark" viewBox="0 0 36 16" aria-hidden="true">
    <polyline {points} />
  </svg>
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

  polyline {
    fill: none;
    stroke: var(--text-soft);
    stroke-width: 1;
    stroke-linecap: round;
    stroke-linejoin: round;
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
