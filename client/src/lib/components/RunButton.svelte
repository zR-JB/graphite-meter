<script lang="ts">
  // Primary start/abort action.
  import { store } from "../state/store.svelte";
  import { engage } from "../runner/engine.svelte";
  import { tooltip } from "../actions/tooltip";
  import { ICON } from "../constants";

  // Once complete, show "Run again" instead of "Engage" to make re-running
  // obvious. Pairs with R key + the ShortcutHints strip.
  const resolved = $derived(
    store.phase === "complete" ||
      store.phase === "aborted" ||
      store.phase === "error",
  );
  const label = $derived(
    store.isRunning
      ? "Abort test"
      : resolved
        ? "Run the test again"
        : "Start the speed test",
  );
</script>

<button
  class="engage"
  class:running={store.isRunning}
  aria-label={label}
  onclick={engage}
  use:tooltip={store.isRunning
    ? "Stop the test (Space / Esc)"
    : resolved
      ? "Run the test again (Space / R)"
      : "Start the test (Space)"}
>
  {#if store.isRunning}
    <span class="stop-sq"></span> ABORT
  {:else if resolved}
    <span class="ico">{@html ICON.bolt}</span> RUN AGAIN
  {:else}
    <span class="ico">{@html ICON.bolt}</span> ENGAGE
  {/if}
</button>

<style>
  .engage {
    position: relative;
    isolation: isolate;
    overflow: hidden;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    /* The one pill — Faceplate's single bold element. Prominent but not a
       full-bleed slab; centered under the gauge with a sensible max. The lit
       top edge + brand glow are the only place we spend this much elevation. */
    width: 100%;
    max-width: 320px;
    align-self: center;
    min-height: 46px;
    border-radius: var(--r-pill);
    font-family: var(--font-display);
    font-weight: 600;
    letter-spacing: var(--track-wide);
    background: linear-gradient(180deg, var(--brand-strong), var(--brand));
    color: var(--text-inverse);
    border: 1px solid color-mix(in srgb, var(--brand) 42%, var(--border));
    box-shadow:
      inset 0 1px 0 var(--edge-highlight),
      0 8px 24px color-mix(in srgb, var(--brand) 24%, transparent);
    cursor: pointer;
    transition:
      transform var(--dur-hover) var(--ease-out),
      filter var(--dur-hover) var(--ease-out);
  }
  .engage:hover {
    transform: translateY(-1px);
    filter: brightness(1.04);
  }
  .engage:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  .engage.running {
    background: var(--err-soft);
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 40%, var(--border));
    box-shadow: none;
  }
  .stop-sq {
    width: 12px;
    height: 12px;
    background: currentColor;
    border-radius: var(--r-well);
  }
  .ico {
    display: inline-grid;
    place-items: center;
    width: 18px;
    height: 18px;
  }
  .ico :global(svg) {
    width: 18px;
    height: 18px;
  }
</style>
