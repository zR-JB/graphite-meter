<script lang="ts">
  /* ============================================================
   * <EngageButton> — the master action (§3.5)
   * Toggles engage/abort against the runner via the wire seam.
   * ============================================================ */
  import { console as store } from "../state/console.svelte";
  import { engage } from "../runner/wire.svelte";
  import { pointerIntent } from "../actions/pointerIntent";
  import { ICON } from "../constants";
</script>

<button
  class="engage"
  class:running={store.isRunning}
  onclick={engage}
  use:pointerIntent
>
  {#if store.isRunning}
    <span class="stop-sq"></span> ABORT
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
    gap: 10px;
    width: 100%;
    min-height: 52px;
    border-radius: var(--radius-md);
    font-family: var(--font-mono);
    font-weight: 700;
    letter-spacing: 0.08em;
    background: linear-gradient(180deg, var(--brand-strong), var(--brand));
    color: var(--text-inverse);
    border: 1px solid color-mix(in srgb, var(--brand) 42%, var(--border));
    box-shadow: 0 8px 24px color-mix(in srgb, var(--brand) 24%, transparent);
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
