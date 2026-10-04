<script lang="ts">
  // The visible text is the accessible name (WCAG 2.5.3); capitals are styling.
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  import { tooltipAction } from "../actions/tooltip";
  import { fmtDuration } from "../format";
  import { handoff } from "../presentation/motion.svelte";
  import { BLOCKED, runActionLabel } from "../presentation/vocabulary";

  const pending = $derived(store.preparing);
  const idle = $derived(!store.isRunning && !pending);
  const eta = $derived(fmtDuration(store.totalEtaMs, 0));
  const action = handoff(
    () => ({
      label: runActionLabel(pending, store.isRunning, store.phase),
      running: store.isRunning,
      pending,
      eta: idle ? eta : "",
    }),
    (shown) => shown.label,
  );
  const { label, running } = $derived(action.shown);
  // The skins crossfade through the handoff: half-way at its swap, settled once it has faded in.
  const stop = $derived(
    running ? (1 + action.opacity) / 2 : (1 - action.opacity) / 2,
  );
  const blocker = $derived(
    idle && !store.catalogLoading ? store.startBlocker : "",
  );
</script>

<button
  class="run-button"
  class:running
  class:pending={action.shown.pending}
  aria-busy={pending}
  aria-disabled={!!blocker}
  aria-describedby={idle ? "run-duration" : undefined}
  style:--stop={stop}
  onclick={controller.toggleRun}
  use:tooltipAction={blocker}
>
  <span class="skin" aria-hidden="true"></span>
  <span class="skin stop" aria-hidden="true"></span>
  <span class="run-button-content" style:opacity={action.opacity}>
    {#if running}
      <span class="stop-sq" aria-hidden="true"></span>
    {/if}
    {label}
  </span>
  {#if action.shown.eta}
    <span class="duration" aria-hidden="true" style:opacity={action.opacity}
      >~{action.shown.eta}</span
    >
  {/if}
</button>
{#if idle}
  <span id="run-duration" class="sr-only"
    >{blocker ? `${BLOCKED}: ${blocker}` : `Estimated duration ${eta}`}</span
  >
{/if}

<style>
  /* Graphite: the one primary action is ink, like every selected control; Stop steps back to an outline. */
  .run-button {
    position: relative;
    isolation: isolate;
    overflow: hidden;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    width: 100%;
    max-width: 320px;
    min-height: 46px;
    padding-inline: var(--space-4);
    border: 0;
    border-radius: var(--r-surface);
    background: none;
    color: color-mix(
      in oklab,
      var(--text-inverse) calc(100% * (1 - var(--stop))),
      var(--text)
    );
    font: var(--w-strong) var(--type-md) / 1 var(--font-sans);
    transition: transform var(--dur-hover) var(--ease-out);
  }
  /* Hover strengthens the skin itself; a filter would re-rasterize the label. */
  @media (hover: hover) {
    .run-button:hover:not(.pending, [aria-disabled="true"]) .skin {
      background: var(--brand-strong);
    }
    .run-button:hover:not(.pending, [aria-disabled="true"]) .skin.stop {
      background: none;
      box-shadow: inset 0 0 0 1px var(--field-edge);
    }
  }
  .run-button:active {
    transform: scale(0.985);
  }
  /* On a phone it spans the run bar at a thumb's height. */
  @container viz (max-width: 520px) {
    .run-button {
      height: var(--hit);
    }
  }
  /* The one solid control, with a lit top edge like every plate. */
  .skin {
    position: absolute;
    inset: 0;
    z-index: -1;
    border-radius: inherit;
    background: var(--brand);
    box-shadow: inset 0 1px 0 rgb(255 255 255 / 0.22);
    opacity: calc(1 - var(--stop));
    transition:
      background-color var(--dur-hover) var(--ease-out),
      box-shadow var(--dur-hover) var(--ease-out);
  }
  .skin.stop {
    background: none;
    box-shadow: inset 0 0 0 1px var(--border-strong);
    opacity: var(--stop);
  }
  .run-button[aria-disabled="true"] {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .run-button-content {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
  .stop-sq {
    width: 10px;
    height: 10px;
    border-radius: 1px;
    background: currentColor;
  }
  /* The estimate is a small chip at the label's side, in the button's own ink. */
  .duration {
    padding: 3px 6px;
    border: var(--hairline) solid
      color-mix(in oklab, currentColor 22%, transparent);
    border-radius: var(--r-well);
    background: color-mix(in oklab, currentColor 9%, transparent);
    color: color-mix(in oklab, currentColor 80%, transparent);
    font: 500 var(--type-2xs) / 1 var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
</style>
