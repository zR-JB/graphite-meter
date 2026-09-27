<script lang="ts">
  import { fly } from "svelte/transition";
  import { fmtDuration } from "../../format";
  import { still } from "../../presentation/motion.svelte";

  let {
    stages,
  }: {
    stages: readonly { key: string; label: string; tone: string; ms: number }[];
  } = $props();
  // A new time rolls in from below as the old one leaves upward, like a counter.
  const roll = (node: Element, { y }: { y: number }) =>
    fly(node, { y, duration: still() ? 0 : 320, opacity: 0 });
</script>

<div class="strip" role="list" aria-label="Stage times">
  {#each stages as stage (stage.key)}
    {@const [value, unit] = fmtDuration(stage.ms).split(" ")}
    <div
      class="segment"
      role="listitem"
      data-tone={stage.tone}
      style:flex-grow={stage.ms}
    >
      <span class="bar" aria-hidden="true"></span>
      <span class="time">
        {#key value}
          <span class="value" in:roll={{ y: 10 }} out:roll={{ y: -10 }}
            >{value}<span class="unit">{unit}</span></span
          >
        {/key}
      </span>
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
  .segment {
    display: grid;
    flex-basis: 0;
    min-width: 52px;
    gap: 2px;
    transition: flex-grow 420ms var(--ease-out);
  }
  .bar {
    height: 4px;
    margin-bottom: 6px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  /* Old and new values share one cell while they roll. */
  .time {
    display: grid;
    overflow: hidden;
  }
  .value {
    grid-area: 1 / 1;
    font: var(--w-normal) var(--type-md) / 1.2 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .unit {
    margin-left: 2px;
    color: var(--text-soft);
    font-size: var(--type-sm);
  }
  .name {
    overflow: hidden;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
