<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtDuration } from "../format";
  import { BUILD } from "../buildenv";
  import { handoff, type Handoff } from "../presentation/motion.svelte";
  import type { Phase } from "../runner/contract";
  import { CONNECTIVITY } from "../presentation/vocabulary";

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
  const left = handoff(
    () => ({ show: showRemaining, recovering, ms: remainingMs }),
    (shown) => `${shown.show}:${shown.recovering}`,
  );
  const { status } = $derived(store.preparation);
  const refused = $derived(status === "blocked" || status === "failed");
</script>

<span
  class="label handoff"
  class:handoff-out={label.out}
  {@attach refused
    ? tooltip(() => store.startError || store.startBlocker)
    : null}>{label.shown.label}</span
>
<span class="elapsed" class:secondary={left.shown.show}
  ><span class="readout">{fmtDuration(elapsedMs)}</span></span
>
<span class="transferred"
  ><span class="readout"
    >{fmtBytes(store.bytesTransferred, store.unitBase)}</span
  ></span
>
{#if left.shown.show}
  <span
    class="remaining handoff"
    class:handoff-out={left.out}
    data-tone={left.shown.recovering ? CONNECTIVITY.recovering.tone : undefined}
  >
    {#if left.shown.recovering}{CONNECTIVITY.recovering.label}, {fmtDuration(
        left.shown.ms,
      )} left{:else}<span class="readout">{fmtDuration(left.shown.ms)}</span>
      left{/if}
  </span>
{/if}
<span class="build">{BUILD.identity}</span>

<style>
  span {
    white-space: nowrap;
  }
  /* The phase word and its figures read as one line from the left edge, each figure in a cell as wide as its
     longest value, so a counting figure never moves its neighbours; the build alone stands at the right. */
  .label {
    color: var(--text);
    font-weight: var(--w-strong);
  }
  .elapsed {
    margin-left: var(--space-2);
  }
  .readout {
    display: inline-block;
    min-width: 7ch;
    color: var(--text-muted);
    font: var(--role-figure-sm);
    line-height: 1;
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
    .transferred {
      display: none;
    }
    .elapsed.secondary {
      display: none;
    }
  }
</style>
