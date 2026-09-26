<script lang="ts">
  // Mirrors Console's shortcuts; R is an alias for Space.
  import { store } from "../state/store.svelte";
  import { RUN_ACTION, runActionLabel } from "../presentation/vocabulary";

  const primary = $derived(
    runActionLabel(store.preparing, store.isRunning, store.phase),
  );
</script>

<div class="command-hints" role="group" aria-label="Keyboard shortcuts">
  <span
    ><kbd>Space</kbd><span class="stack">
      {#each Object.values(RUN_ACTION) as label (label)}
        <span class:current={label === primary} aria-hidden={label !== primary}
          >{label}</span
        >
      {/each}
    </span></span
  >
  <span><kbd>S</kbd>Settings</span>
  <span><kbd>D</kbd>Details</span>
  {#if store.savingResults}
    <span><kbd>H</kbd>History</span>
  {/if}
</div>

<style>
  .command-hints {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-4);
    color: var(--text-soft);
    font: var(--w-normal) var(--type-xs) var(--font-sans);
  }
  span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  /* Every run action shares one cell, so a label change never moves the strip. */
  .stack {
    display: grid;
  }
  .stack > * {
    grid-area: 1 / 1;
  }
  .stack > :not(.current) {
    visibility: hidden;
  }
  /* The status strip has no room for keycaps on narrow screens. */
  @container status (max-width: 1100px) {
    .command-hints {
      display: none;
    }
  }
</style>
