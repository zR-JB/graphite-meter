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
  <span><kbd>M</kbd>{store.uxMode === "simple" ? "Advanced" : "Simple"}</span>
  {#if store.phase === "complete"}
    <span><kbd>R</kbd>Run again</span>
  {/if}
</div>

<style>
  .command-hints {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 700;
  }

  span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: color-mix(in srgb, var(--surface-inset) 84%, transparent);
    padding: 4px 8px;
  }

  kbd {
    display: inline-grid;
    min-width: 20px;
    height: 18px;
    place-items: center;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-xs);
    background: linear-gradient(180deg, var(--surface-2), var(--surface-1));
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 700;
    box-shadow: inset 0 -1px 0 var(--border-subtle);
  }

  /* The 28px status zone has no room for keycaps on a phone; the
     visible CommandHints strip is a desktop affordance. */
  @media (max-width: 759px) {
    .command-hints {
      display: none;
    }
  }
</style>
