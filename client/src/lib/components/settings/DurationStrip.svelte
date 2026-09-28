<script lang="ts">
  import Roll from "../Roll.svelte";
  import { fmtStageTime } from "../../format";

  let {
    stages,
  }: {
    stages: readonly { key: string; label: string; tone: string; ms: number }[];
  } = $props();
</script>

<div class="strip" role="list" aria-label="Stage times">
  {#each stages as stage (stage.key)}
    <div
      class="segment"
      role="listitem"
      data-tone={stage.tone}
      style:flex-grow={stage.ms}
    >
      <span class="bar" aria-hidden="true"></span>
      <span class="time"
        ><Roll text={fmtStageTime(stage.ms)} rank={stage.ms} /></span
      >
      <span class="name">{stage.label}</span>
    </div>
  {/each}
</div>

<style>
  .strip {
    display: flex;
    gap: 3px;
    padding-block: var(--space-3) 10px;
  }
  /* Widths follow the times, but never below a segment's own label. */
  .segment {
    display: grid;
    flex-basis: 0;
    gap: 2px;
    transition: flex-grow 420ms var(--ease-out);
  }
  .bar {
    height: 4px;
    margin-bottom: 6px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  /* A segment's words keep a space before the next segment's. */
  .time,
  .name {
    padding-inline-end: var(--space-2);
    white-space: nowrap;
  }
  .time {
    font: var(--w-normal) var(--type-md) / 1.2 var(--font-sans);
  }
  .name {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
  }
</style>
