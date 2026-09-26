<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtDuration } from "../format";
  import { BUILD } from "../buildenv";
  import { BLOCKED, phaseLabel } from "../presentation/vocabulary";

  // Progress events pace the wall clock, so elapsed and left update together.
  const elapsedMs = $derived.by(() => {
    if (store.result) return store.result.durationMs;
    if (!store.startEpoch) return 0;
    void store.phaseElapsedMs;
    return Date.now() - store.startEpoch;
  });

  const showRemaining = $derived(store.isRunning && store.phaseBudgetMs > 0);
</script>

{#if store.preparation.status === "blocked"}
  <span class="label term" use:tooltip={store.startError || store.startBlocker}
    >{BLOCKED}</span
  >
{:else}
  <span class="label">{phaseLabel(store.phase, store.result?.outcome)}</span>
{/if}
<span
  class="elapsed"
  class:secondary={showRemaining}
  use:tooltip={`Elapsed ${fmtDuration(elapsedMs)}`}
  ><span class="caption">elapsed&nbsp;</span><span class="readout"
    >{fmtDuration(elapsedMs)}</span
  ></span
>
<span class="transferred"
  ><span class="readout"
    >{fmtBytes(store.bytesTransferred, store.unitBase)}</span
  ><span class="caption">&nbsp;transferred</span></span
>
{#if showRemaining}
  <span class="remaining" class:paused={!store.measuring}>
    {#if store.measuring}<span class="readout"
        >{fmtDuration(store.phaseRemainingMs)}</span
      > left{:else}Paused<span class="caption">
        · {fmtDuration(store.phaseRemainingMs)} left</span
      >{/if}
  </span>
{/if}
<span class="build">{BUILD.identity}</span>

<style>
  span {
    white-space: nowrap;
  }
  /* Reserved widths keep a changing phase or number from moving its neighbours;
     the countdown comes last so its arrival and exit move nothing. */
  .label {
    min-width: 14ch;
    color: var(--text);
    font-weight: var(--w-strong);
  }
  .readout {
    display: inline-block;
    min-width: 7ch;
    font-variant-numeric: tabular-nums;
    text-align: end;
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
