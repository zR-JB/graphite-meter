<script lang="ts">
  // Bottom status strip: phase label, elapsed/remaining time, transferred bytes,
  // build identity, and compact connection hints.
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { fmtBytes } from "../format";
  import { BUILD_HASH } from "../constants";
  import type { Phase } from "../runner/contract";

  const PHASE_LABEL: Record<Phase, string> = {
    idle: "Idle",
    connecting: "Verifying target",
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
  let visible = $state(true);

  onMount(() => {
    const update = () => (visible = !document.hidden);
    update();
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  });

  $effect(() => {
    if (!store.isRunning || !visible) return;
    now = Date.now();
    const id = setInterval(() => (now = Date.now()), 200);
    return () => clearInterval(id);
  });

  const elapsedMs = $derived(
    store.isRunning && store.startEpoch
      ? now - store.startEpoch
      : (store.result?.durationMs ??
          (store.phase === "aborted" && store.startEpoch
            ? now - store.startEpoch
            : 0)),
  );

  function fmtElapsed(ms: number): string {
    const s = ms / 1000;
    return `${s.toFixed(1)}s`;
  }

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
  .paused {
    color: var(--err);
    font-weight: 600;
  }
</style>
