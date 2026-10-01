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
      style:--ms={stage.ms}
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
    gap: 2px;
    padding-block: var(--space-4) var(--space-3);
  }
  /* Widths follow the times, but never below a segment's own label. */
  .segment {
    display: grid;
    flex: var(--ms) 1 0;
    gap: 2px;
    transition: flex-grow var(--dur-graph) var(--ease-out);
  }
  /* A stage that joins grows from its label's width instead of popping in at its share. */
  @starting-style {
    .segment {
      flex-grow: 0;
    }
  }
  /* The hues are data, so forced colours keep them. */
  .bar {
    height: 4px;
    margin-bottom: 6px;
    border-radius: var(--r-full);
    background: var(--tone);
    forced-color-adjust: none;
  }
  /* A segment's words keep a space before the next segment's. */
  .time,
  .name {
    padding-inline-end: var(--space-2);
    white-space: nowrap;
  }
  .time {
    font: var(--w-normal) var(--type-md) / 16px var(--font-sans);
  }
  .name {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 16px var(--font-sans);
  }
</style>
