<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtDuration } from "../format";
  import { BUILD } from "../buildenv";
  import type { Handoff } from "../presentation/motion.svelte";
  import type { Phase } from "../runner/contract";
  import { CONNECTIVITY, STATUS_LABEL_CH } from "../presentation/vocabulary";

  let { status: label }: { status: Handoff<{ phase: Phase; label: string }> } =
    $props();

  const elapsedMs = $derived(
    store.result?.durationMs ?? store.runClock.current,
  );
  const remainingMs = $derived(
    Math.max(0, store.phaseBudgetMs - store.phaseClock.current),
  );

  const showRemaining = $derived(store.isRunning && store.phaseBudgetMs > 0);
  const recovering = $derived(store.effectiveConnectivity === "recovering");
  const { status } = $derived(store.preparation);
  const refused = $derived(status === "blocked" || status === "failed");
</script>

<span
  class="label"
  style:opacity={label.opacity}
  style:min-width="{STATUS_LABEL_CH}ch"
  {@attach refused
    ? tooltip(() => store.startError || store.startBlocker)
    : null}>{label.shown.label}</span
>
<span class="elapsed" class:secondary={showRemaining}
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
  <span
    class="remaining"
    data-tone={recovering ? CONNECTIVITY.recovering.tone : undefined}
  >
    {#if recovering}{CONNECTIVITY.recovering.label}<span class="caption">
        · {fmtDuration(remainingMs)} left</span
      >{:else}<span class="readout">{fmtDuration(remainingMs)}</span> left{/if}
  </span>
{/if}
<span class="build">{BUILD.identity}</span>

<style>
  span {
    white-space: nowrap;
  }
  /* Fixed widths and a trailing countdown keep changing text from moving the strip. */
  .label {
    color: var(--text);
    font-weight: var(--w-strong);
  }
  .readout {
    display: inline-block;
    min-width: 9ch;
    font-variant-numeric: tabular-nums;
    text-align: end;
  }
  .build {
    margin-left: auto;
    color: var(--text-soft);
  }
  .remaining[data-tone] {
    color: var(--tone);
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
