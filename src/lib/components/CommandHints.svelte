<script lang="ts">
  /* ============================================================
   * <CommandHints> — contextual keyboard-shortcut strip (§7 + §13.7)
   * A subtle, tokenized row of keycaps that mirrors the global
   * keyboard map in <Console>. The primary hint flips with run
   * state (Space = Engage / Abort), and an "R · Run again" cap
   * appears only once a run has resolved. Decorative + advisory:
   * the real handler lives in Console.
   * ============================================================ */
  import { console as store } from "../state/console.svelte";

  const primary = $derived(store.isRunning ? "Abort" : "Engage");
</script>

<div class="command-hints" aria-label="Keyboard shortcuts">
  <span><kbd>Space</kbd>{primary}</span>
  <span><kbd>W</kbd>Workbench</span>
  <span><kbd>D</kbd>Details</span>
  {#if store.phase === "complete"}
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
     visible CommandHints strip is a desktop affordance. */
  @media (max-width: 759px) {
    .command-hints {
      display: none;
    }
  }
</style>
