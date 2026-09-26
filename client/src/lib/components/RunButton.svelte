<script lang="ts">
  // The visible text is the accessible name (WCAG 2.5.3); capitals are styling.
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  import { tooltip } from "../actions/tooltip";
  import { ICON } from "../constants";
  import { fmtDuration } from "../format";
  import { resolvedPhase, runActionLabel } from "../presentation/vocabulary";

  const pending = $derived(store.preparing);
  const resolved = $derived(resolvedPhase(store.phase));
  const label = $derived(runActionLabel(pending, store.isRunning, store.phase));
  const eta = $derived(fmtDuration(store.totalEtaMs, 0));
</script>

<button
  class="run-button"
  class:running={store.isRunning}
  class:pending
  aria-busy={pending}
  aria-describedby={!store.isRunning && !pending ? "run-duration" : undefined}
  onclick={controller.toggleRun}
  use:tooltip={pending
    ? "Cancel starting the test (Space / Esc)"
    : store.isRunning
      ? "Stop the test (Space / Esc)"
      : resolved
        ? "Run the test again (Space / R)"
        : "Start the test (Space)"}
>
  {#key label}
    <span class="run-button-content enter">
      {#if store.isRunning}
        <span class="stop-sq" aria-hidden="true"></span>
      {:else if !pending}
        <span class="ico" aria-hidden="true">{@html ICON.bolt}</span>
      {/if}
      {label}
    </span>
  {/key}
  {#if !store.isRunning && !pending}
    <span class="duration" aria-hidden="true">~{eta}</span>
  {/if}
</button>
{#if !store.isRunning && !pending}
  <span id="run-duration" class="sr-only">Estimated duration {eta}</span>
{/if}

<style>
  .run-button {
    position: relative;
    isolation: isolate;
    overflow: hidden;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    max-width: 320px;
    min-height: 46px;
    align-self: center;
    border: 1px solid var(--brand-line);
    border-radius: var(--r-pill);
    background: linear-gradient(180deg, var(--brand-strong), var(--brand));
    box-shadow:
      inset 0 1px 0 var(--edge-highlight),
      0 2px 8px color-mix(in srgb, var(--brand) 10%, transparent);
    color: var(--text-inverse);
    font-family: var(--font-display);
    font-weight: var(--w-strong);
    letter-spacing: var(--track-wide);
    text-transform: uppercase;
    transition:
      transform var(--dur-hover) var(--ease-out),
      filter var(--dur-hover) var(--ease-out);
  }
  @media (hover: hover) {
    .run-button:hover:not(.pending) {
      transform: translateY(-1px);
      filter: brightness(1.04);
    }
  }
  .run-button:active {
    transform: scale(0.985);
  }
  .run-button.running {
    border-color: var(--err-line);
    background: var(--err-soft);
    box-shadow: none;
    color: var(--err);
  }
  .run-button.pending {
    filter: saturate(0.7);
  }
  .run-button-content {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
  .run-button-content :global(svg) {
    width: 18px;
    height: 18px;
  }
  .stop-sq {
    width: 12px;
    height: 12px;
    border-radius: var(--r-well);
    background: currentColor;
  }
  .duration {
    position: absolute;
    inset-inline-end: var(--space-3);
    padding: var(--space-1) 6px;
    border: 1px solid color-mix(in srgb, currentColor 20%, transparent);
    border-radius: var(--r-well);
    background: color-mix(in srgb, currentColor 8%, transparent);
    font: var(--w-normal) var(--type-2xs) / 1 var(--font-mono);
    letter-spacing: 0;
    text-transform: none;
  }
</style>
