<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtDuration } from "../format";
  import { BUILD } from "../buildenv";
  import { phaseLabel } from "../presentation/vocabulary";

  let now = $state(Date.now());
  let visible = $state(typeof document === "undefined" || !document.hidden);

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

  const showRemaining = $derived(store.isRunning && store.phaseBudgetMs > 0);
</script>

<svelte:document onvisibilitychange={() => (visible = !document.hidden)} />

<span class="label">{phaseLabel(store.phase, store.result?.outcome)}</span>
<span
  class="elapsed"
  class:secondary={showRemaining}
  use:tooltip={`Elapsed ${fmtDuration(elapsedMs)}`}
  ><span class="caption">elapsed&nbsp;</span>{fmtDuration(elapsedMs)}</span
>
{#if showRemaining}
  <span class="remaining" class:paused={!store.measuring}>
    {#if store.measuring}{fmtDuration(store.phaseRemainingMs)} left{:else}Paused<span
        class="caption"
      >
        · {fmtDuration(store.phaseRemainingMs)} left</span
      >{/if}
  </span>
{/if}
<span class="transferred"
  >{fmtBytes(store.bytesTransferred, store.unitBase)}<span class="caption"
    >&nbsp;transferred</span
  ></span
>
<span class="build">{BUILD.identity}</span>

<style>
  span {
    white-space: nowrap;
  }
  .label {
    color: var(--text);
    font-weight: var(--w-strong);
  }
  .build {
    margin-left: auto;
    color: var(--text-soft);
  }
  .paused {
    color: var(--err);
    font-weight: var(--w-strong);
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
