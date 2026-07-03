<script lang="ts">
  /* ============================================================
   * <StatusBar> — bottom status zone
   * State-machine label, elapsed, bytes transferred, build hash.
   * Owns a local ticker so elapsed advances continuously (off a
   * wall clock) rather than only when a sample lands.
   * ============================================================ */
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { fmtBytes } from "../format";
  import { BUILD_HASH } from "../constants";
  import type { Phase } from "../runner/contract";

  const PHASE_LABEL: Record<Phase, string> = {
    idle: "Idle",
    warmup: "Warming up",
    latency: "Measuring latency",
    download: "Downloading",
    upload: "Uploading",
    bidirectional: "Bidirectional",
    complete: "Complete",
    aborted: "Aborted",
    error: "Error",
  };

  let now = $state(Date.now());
  onMount(() => {
    const id = setInterval(() => (now = Date.now()), 200);
    return () => clearInterval(id);
  });

  const elapsedMs = $derived(
    store.isRunning && store.startEpoch
      ? now - store.startEpoch
      : (store.result?.durationMs ?? 0),
  );

  function fmtElapsed(ms: number): string {
    const s = ms / 1000;
    return `${s.toFixed(1)}s`;
  }

  // Test-time remaining in the active measured phase (budget − measured
  // elapsed). It STOPS shrinking while stalled (both inputs freeze in the
  // core), so a drop makes the push-out of the run end visible here. Shown
  // only while running with a real budget; on a stall it's tagged "paused".
  const showRemaining = $derived(store.isRunning && store.phaseBudgetMs > 0);
</script>

<span class="label" role="status" aria-live="polite"
  >{PHASE_LABEL[store.phase]}</span
>
<span class="sep">·</span>
<span>elapsed {fmtElapsed(elapsedMs)}</span>
{#if showRemaining}
  <span class="sep">·</span>
  <span class:paused={!store.measuring}>
    {fmtElapsed(store.phaseRemainingMs)} left{#if !store.measuring}&nbsp;(paused){/if}
  </span>
{/if}
<span class="sep">·</span>
<span>{fmtBytes(store.bytesTransferred, store.unitBase)} xfer</span>
<span class="flex-1"></span>
<span class="soft">build {BUILD_HASH}</span>

<style>
  /* Keep each status token on one line; the strip clips (Console .status) if
     the row is too narrow, so nothing wraps past the fixed 28px height. */
  span {
    white-space: nowrap;
  }
  .label {
    color: var(--text);
    font-weight: 600;
  }
  .sep {
    color: var(--text-soft);
  }
  .flex-1 {
    flex: 1;
  }
  .soft {
    color: var(--text-soft);
  }
  /* Remaining-time stalls: tint amber/error so the frozen countdown reads as
     "paused — link down", not a normal tick. */
  .paused {
    color: var(--err);
    font-weight: 600;
  }
</style>
