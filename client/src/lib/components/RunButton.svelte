<script lang="ts">
  // Primary start/abort action.
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  import { tooltip } from "../actions/tooltip";
  import { ICON } from "../constants";

  // A resolved run relabels the button "Run again", pairing with the R key
  // and the ShortcutHints strip.
  const pending = $derived(store.preparing);
  const resolved = $derived(
    store.phase === "complete" ||
      store.phase === "aborted" ||
      store.phase === "error",
  );
  const label = $derived(
    pending
      ? "Cancel"
      : store.isRunning
        ? "Abort test"
        : resolved
          ? "Run the test again"
          : "Start the speed test",
  );
</script>

<button
  class="run-button"
  class:running={store.isRunning}
  class:pending
  aria-label={label}
  aria-busy={pending}
  onclick={controller.toggleRun}
  use:tooltip={pending
    ? "Cancel starting the test (Space / Esc)"
    : store.isRunning
      ? "Stop the test (Space / Esc)"
      : resolved
        ? "Run the test again (Space / R)"
        : "Start the test (Space)"}
>
  <span class="run-button-content">
    {#if pending}
      CANCEL
    {:else if store.isRunning}
      <span class="stop-sq"></span> ABORT
    {:else if resolved}
      <span class="ico">{@html ICON.bolt}</span> RUN AGAIN
    {:else}
      <span class="ico">{@html ICON.bolt}</span> START TEST
    {/if}
  </span>
</button>

<style>
  .run-button {
    position: relative;
    isolation: isolate;
    overflow: hidden;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    /* The one pill: the faceplate's single bold element, centered under the
       gauge. Its lit top edge and brand glow are the only such elevation. */
    width: 100%;
    max-width: 320px;
    align-self: center;
    height: 46px;
    min-height: 46px;
    box-sizing: border-box;
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
  .run-button:hover {
    transform: translateY(-1px);
    filter: brightness(1.04);
  }
  .run-button:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  .run-button.running {
    background: var(--err-soft);
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 40%, var(--border));
    box-shadow: none;
  }
  .run-button.pending {
    cursor: pointer;
    filter: saturate(0.7);
  }
  .run-button.pending:hover {
    transform: none;
    filter: saturate(0.7);
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
  .run-button-content {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
  }
  @media (prefers-reduced-motion: no-preference) {
    .run-button-content {
      animation: control-content-enter var(--dur-hover) var(--ease-out) both;
    }
  }
  @keyframes control-content-enter {
    from {
      opacity: 0.7;
      transform: translateY(1px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
</style>
