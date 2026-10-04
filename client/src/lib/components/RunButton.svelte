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
  onclick={controller.toggleRun}
  use:tooltipAction={blocker}
>
  <span class="skin" aria-hidden="true"></span>
  <span class="skin stop" aria-hidden="true"></span>
  <span class="run-button-content handoff" class:handoff-out={action.out}>
    {#if running}
      <span class="stop-sq" aria-hidden="true"></span>
    {/if}
    {label}
  </span>
  {#if action.shown.eta}
    <span
      class="duration handoff"
      class:handoff-out={action.out}
      aria-hidden="true">~{action.shown.eta}</span
    >
  {/if}
</button>
{#if idle}
  <span id="run-duration" class="sr-only"
    >{blocker ? `${BLOCKED}: ${blocker}` : `Estimated duration ${eta}`}</span
  >
{/if}

<style>
  /* Graphite: the one primary action is ink, like every selected control; Stop steps back to a quiet plate. The
     key stands under the dial's panel at the panel's width, its label and estimate centred. */
  .run-button {
    position: relative;
    isolation: isolate;
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    width: 100%;
    height: 48px;
    padding-inline: 14px;
    border: 0;
    border-radius: var(--r-chrome);
    background: none;
    color: var(--text-inverse);
    font: var(--w-strong) var(--type-md) / 1 var(--font-sans);
    transition:
      transform var(--dur-hover) var(--ease-out),
      color var(--dur-handoff-in) var(--ease-out);
  }
  .run-button.running {
    color: var(--text);
  }
  /* Hover strengthens the skin itself; a filter would re-rasterize the label. */
  @media (hover: hover) {
    .run-button:hover:not(.pending, [aria-disabled="true"]) .skin {
      background: var(--brand-strong);
    }
    /* The wash lies over the Stop skin's own fill, so the ink skin under it never shows through. */
    .run-button:hover:not(.pending, [aria-disabled="true"]) .skin.stop {
      background: linear-gradient(var(--hover-wash) 0 0), var(--surface-1);
    }
  }
  .run-button:active {
    transform: scale(0.985);
  }
  /* A phone's key keeps a thumb's height. */
  @container viz (max-width: 520px) {
    .run-button {
      height: var(--hit);
    }
  }
  /* The ink skin stays; the Stop skin fades over it with the hand-off, so the key changes in one breath. */
  .skin {
    position: absolute;
    inset: 0;
    z-index: -1;
    border-radius: inherit;
    background: var(--brand);
    transition:
      background-color var(--dur-hover) var(--ease-out),
      box-shadow var(--dur-hover) var(--ease-out),
      opacity var(--dur-handoff-in) var(--ease-out);
  }
  .skin.stop {
    background: var(--surface-1);
    box-shadow: inset 0 0 0 var(--hairline) var(--border-strong);
    opacity: 0;
  }
  .running .skin.stop {
    opacity: 1;
  }
  .run-button[aria-disabled="true"] {
    opacity: 0.6;
    cursor: not-allowed;
  }
  /* When the key becomes Start or Run again, a sheen crosses it once: the run is ready for the next press. */
  @media (prefers-reduced-motion: no-preference) {
    .run-button:not(.running, [aria-disabled="true"])::after {
      content: "";
      position: absolute;
      inset: 0;
      z-index: -1;
      width: 35%;
      background: linear-gradient(
        100deg,
        transparent,
        color-mix(in oklab, var(--text-inverse) 22%, transparent),
        transparent
      );
      animation: glint 1100ms var(--ease-out) 700ms both;
      pointer-events: none;
    }
  }
  .run-button-content {
    display: inline-flex;
    align-items: center;
    gap: 9px;
  }
  .stop-sq {
    width: 9px;
    height: 9px;
    border-radius: 1px;
    background: currentColor;
  }
  .duration {
    font: var(--role-figure-sm);
    line-height: 1;
    font-variant-numeric: tabular-nums;
    opacity: 0.85;
  }
</style>
