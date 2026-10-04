<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtDuration } from "../format";
  import { BUILD } from "../buildenv";
  import { handoff, type Handoff } from "../presentation/motion.svelte";
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
  const counters = handoff(
    () => ({
      run: store.runSeq,
      elapsedMs,
      bytes: fmtBytes(store.bytesTransferred, store.unitBase),
    }),
    (shown) => shown.run,
  );
  const left = handoff(
    () => ({ show: showRemaining, recovering, ms: remainingMs }),
    (shown) => `${shown.show}:${shown.recovering}`,
  );
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
<span
  class="elapsed"
  class:secondary={left.shown.show}
  style:opacity={counters.opacity}
  ><span class="readout">{fmtDuration(counters.shown.elapsedMs)}</span><span
    class="caption">&nbsp;elapsed</span
  ></span
>
<span class="transferred" style:opacity={counters.opacity}
  ><span class="readout">{counters.shown.bytes}</span><span class="caption"
    >&nbsp;transferred</span
  ></span
>
{#if left.shown.show}
  <span
    class="remaining"
    data-tone={left.shown.recovering ? CONNECTIVITY.recovering.tone : undefined}
    style:opacity={left.opacity}
  >
    {#if left.shown.recovering}{CONNECTIVITY.recovering.label}<span
        class="caption">, {fmtDuration(left.shown.ms)} left</span
      >{:else}<span class="readout">{fmtDuration(left.shown.ms)}</span>
      left{/if}
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
    color: var(--text-muted);
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
  /* A phone's strip keeps one time in one place; each card shows what its stage transferred. */
  @container status (max-width: 520px) {
    .caption,
    .transferred {
      display: none;
    }
    .elapsed.secondary {
      display: none;
    }
  }
</style>
