<script lang="ts">
  import { fly } from "svelte/transition";
  import { still } from "../presentation/motion.svelte";

  // A changed time rolls like a counter: up as it grows, down as it shrinks.
  let { text, rank }: { text: string; rank: number } = $props();
  let direction = 1;
  let previous = NaN;
  $effect.pre(() => {
    if (rank !== previous && !Number.isNaN(previous))
      direction = rank > previous ? 1 : -1;
    previous = rank;
  });
  const roll = (node: Element, { side }: { side: 1 | -1 }) =>
    fly(node, {
      y: side * direction * 10,
      duration: still() ? 0 : 320,
      opacity: 0,
    });
  // Figures in ink, units quieter.
  const parts = $derived(text.split(/(\d+(?:\.\d+)?)/).filter(Boolean));
</script>

<span class="roll">
  {#key text}
    <span class="value" in:roll={{ side: 1 }} out:roll={{ side: -1 }}
      >{#each parts as part, index (index)}{#if /\d/.test(part)}{part}{:else}<span
            class="unit">{part}</span
          >{/if}{/each}</span
    >
  {/key}
</span>

<style>
  /* Old and new values share one cell while they roll. */
  .roll {
    display: inline-grid;
    overflow: hidden;
    vertical-align: bottom;
  }
  .value {
    grid-area: 1 / 1;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  /* A unit keeps its spaces, so it reads and copies as "1 min 30 s", and sits within its figures' line. */
  .unit {
    color: var(--text-soft);
    font-size: var(--type-sm);
    line-height: 1;
  }
</style>
