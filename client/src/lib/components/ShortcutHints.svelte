<script lang="ts">
  // Mirrors Console's shortcuts; R is an alias for Space.
  import { store } from "../state/store.svelte";
  import { runActionLabel } from "../presentation/vocabulary";

  const primary = $derived(
    runActionLabel(store.preparing, store.isRunning, store.phase),
  );
</script>

<div class="command-hints" role="group" aria-label="Keyboard shortcuts">
  <span><kbd>Space</kbd>{primary}</span>
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
  /* The status strip has no room for keycaps on narrow screens. */
  @container status (max-width: 1100px) {
    .command-hints {
      display: none;
    }
  }
</style>
