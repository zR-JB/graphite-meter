<script lang="ts">
  /* Contextual keyboard-shortcut strip: a tokenized row of keycaps mirroring
     the global keyboard map in <Console>, which owns the real handler. The
     primary hint flips with run state (Space = Start test or Abort), and the
     "R · Run again" cap appears once a run resolves. */
  import { store } from "../state/store.svelte";

  // Mirror RunButton's label exactly (Start test → Abort → Run again) so the hint
  // never names an action the button doesn't show.
  const resolved = $derived(
    store.phase === "complete" ||
      store.phase === "aborted" ||
      store.phase === "error",
  );
  const primary = $derived(
    store.preparing
      ? "Cancel start"
      : store.isRunning
        ? "Abort"
        : resolved
          ? "Run again"
          : "Start test",
  );
</script>

<div class="command-hints" aria-label="Keyboard shortcuts">
  <span><kbd>Space</kbd>{primary}</span>
  <span><kbd>S</kbd>Settings</span>
  <span><kbd>D</kbd>Info</span>
  {#if store.savingResults}
    <span><kbd>H</kbd>History</span>
  {/if}
  {#if resolved}
    <span><kbd>R</kbd>Run again</span>
  {/if}
</div>

<style>
  /* A quiet row of keycap and label pairs mirroring Console's shortcuts. */
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
