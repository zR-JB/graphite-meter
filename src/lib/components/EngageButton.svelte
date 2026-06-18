<script lang="ts">
  /* ============================================================
   * <EngageButton> — the master action (§3.5)
   * Toggles engage/abort against the runner via the wire seam.
   * ============================================================ */
  import { console as store } from "../state/console.svelte";
  import { engage } from "../runner/wire.svelte";
  import { pointerIntent } from "../actions/pointerIntent";
  import { tooltip } from "../actions/tooltip";
  import { ICON } from "../constants";

  // Once a run has resolved, the master action re-runs the test. Surfacing
  // "Run again" (vs a bare repeat of "Engage") makes that affordance obvious
  // (§14.3) — pairs with the R key + the CommandHints strip.
  const resolved = $derived(
    store.phase === "complete" ||
      store.phase === "aborted" ||
      store.phase === "error",
  );
  const label = $derived(
    store.isRunning ? "Abort test" : resolved ? "Run the test again" : "Start the speed test",
  );
</script>

<button
  class="engage"
  class:running={store.isRunning}
  aria-label={label}
  onclick={engage}
  use:pointerIntent
  use:tooltip={store.isRunning
    ? "Stop the test (Space / Esc)"
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
      inset 0 1px 0 rgba(255, 255, 255, 0.22),
      0 8px 24px color-mix(in srgb, var(--brand) 24%, transparent);
    cursor: pointer;
    transition:
      transform var(--dur-hover) var(--ease-out),
      filter var(--dur-hover) var(--ease-out);
  }
  /* Pointer-follow shimmer (§13.7): a soft radial highlight centered on the
     cursor via pointerIntent's --intent-x/--intent-y. Defaults to the button
     center (50%/50%) until the pointer arrives, fades in on hover. Purely
     decorative — disabled under reduced motion. */
  .engage::after {
    content: "";
    position: absolute;
    inset: 0;
    z-index: -1;
    pointer-events: none;
    opacity: 0;
    background: radial-gradient(
      180px circle at var(--intent-x, 50%) var(--intent-y, 50%),
      color-mix(in srgb, var(--brand-strong) 70%, white) 0%,
      transparent 60%
    );
    transition: opacity var(--dur-hover) var(--ease-out);
  }
  .engage:hover::after {
    opacity: 0.5;
  }
  .engage.running::after {
    display: none;
  }
  .engage:hover {
    transform: translateY(-1px);
    filter: brightness(1.04);
  }
  @media (prefers-reduced-motion: reduce) {
    .engage::after {
      display: none;
    }
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
    border-radius: var(--radius-xs);
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
