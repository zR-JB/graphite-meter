<script lang="ts">
  // Bottom status strip: phase label, elapsed/remaining time, transferred bytes,
  // build identity, and compact connection hints.
  import { tooltip } from "../actions/tooltip";
  import { onMount } from "svelte";
  import { store } from "../state/store.svelte";
  import { fmtBytes } from "../format";
  import { BUILD_IDENTITY } from "../constants";
  import type { Phase } from "../runner/contract";
  import { completionLabel } from "./phasePresentation";

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
  >{store.phase === "complete"
    ? completionLabel(store.result?.outcome)
    : PHASE_LABEL[store.phase]}</span
>
<span
  class="elapsed"
  class:secondary={showRemaining}
  use:tooltip={`Elapsed ${fmtElapsed(elapsedMs)}`}
  ><span class="caption">elapsed&nbsp;</span>{fmtElapsed(elapsedMs)}</span
>
{#if showRemaining}
  <span class="remaining" class:paused={!store.measuring}>
    {#if store.measuring}{fmtElapsed(store.phaseRemainingMs)} left{:else}Paused<span
        class="caption"
      >
        · {fmtElapsed(store.phaseRemainingMs)} left</span
      >{/if}
  </span>
{/if}
<span class="transferred"
  >{fmtBytes(store.bytesTransferred, store.unitBase)}<span class="caption">
    xfer</span
  ></span
>
<span class="build">{BUILD_IDENTITY}</span>

<style>
  span {
    white-space: nowrap;
  }
  .label {
    color: var(--text);
    font-weight: 600;
    margin-right: auto;
  }
  .build {
    margin-left: auto;
    color: var(--text-soft);
  }
  .paused {
    color: var(--err);
    font-weight: 600;
  }
  @container status (max-width: 800px) {
    .build {
      display: none;
    }
  }
  @container status (max-width: 520px) {
    .caption {
      display: none;
    }
    .elapsed.secondary {
      display: none;
    }
  }
  @container status (max-width: 350px) {
    .transferred {
      display: none;
    }
  }
</style>
