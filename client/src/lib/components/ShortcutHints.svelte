<script lang="ts">
  /* ============================================================
   * <ShortcutHints> — contextual keyboard-shortcut strip
   * A subtle, tokenized row of keycaps that mirrors the global
   * keyboard map in <Console>. The primary hint flips with run
   * state (Space = Engage / Abort), and an "R · Run again" cap
   * appears only once a run has resolved. Decorative + advisory:
   * the real handler lives in Console.
   * ============================================================ */
  import { store } from "../state/store.svelte";

  // Mirror RunButton's label exactly (Engage → Abort → Run again) so the hint
  // never names an action the button doesn't show.
  const resolved = $derived(
    store.phase === "complete" ||
      store.phase === "aborted" ||
      store.phase === "error",
  );
  const primary = $derived(
    store.isRunning ? "Abort" : resolved ? "Run again" : "Engage",
  );
</script>

<div class="command-hints" aria-label="Keyboard shortcuts">
  <span><kbd>Space</kbd>{primary}</span>
  <span><kbd>W</kbd>Settings</span>
  <span><kbd>D</kbd>Info</span>
  {#if resolved}
    <span><kbd>R</kbd>Run again</span>
  {/if}
</div>

<style>
  /* A quiet row of keycap + label pairs. No pill container (that read cheap and
     cramped) — just a single clean keycap in the Faceplate tile language and a
     muted label, with generous spacing between groups. */
  .command-hints {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-4);
    color: var(--text-soft);
    font-family: var(--font-sans);
    font-size: 10.5px;
    font-weight: 500;
    letter-spacing: 0.01em;
  }

  span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 16px;
    height: 15px;
    padding: 0 4px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--surface-2);
    box-shadow: var(--elev-tile);
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0;
  }

  /* The 28px status zone has no room for keycaps on a phone; the
     visible ShortcutHints strip is a desktop affordance. */
  @media (max-width: 759px) {
    /* bp: stacked */
    .command-hints {
      display: none;
    }
  }
</style>
