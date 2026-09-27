<script lang="ts">
  import Icon from "./Icon.svelte";
  // The visible text is the accessible name (WCAG 2.5.3); capitals are styling.
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  import { tooltip } from "../actions/tooltip";
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
  {@attach tooltip(() => blocker)}
>
  <span class="skin" aria-hidden="true"></span>
  <span class="skin stop" aria-hidden="true"></span>
  <span class="run-button-content" style:opacity={action.opacity}>
    {#if running}
      <span class="stop-sq" aria-hidden="true"></span>
    {:else if !action.shown.pending}
      <span class="ico" aria-hidden="true"><Icon name="bolt" /></span>
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
    border: 0;
    border-radius: var(--r-pill);
    background: none;
    box-shadow: 0 2px 8px
      color-mix(
        in srgb,
        var(--brand) calc(10% * (1 - var(--stop))),
        transparent
      );
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
    .run-button:hover:not(.pending, [aria-disabled="true"]) {
      transform: translateY(-1px);
      filter: brightness(1.04);
    }
  }
  .run-button:active {
    transform: scale(0.985);
  }
  .skin {
    position: absolute;
    inset: 0;
    z-index: -1;
    border: 1px solid var(--brand-line);
    border-radius: inherit;
    background: linear-gradient(180deg, var(--brand-strong), var(--brand));
    box-shadow: inset 0 1px 0 var(--edge-highlight);
    opacity: calc(1 - var(--stop));
  }
  .skin.stop {
    border-color: var(--err-line);
    background: var(--err-soft);
    box-shadow: none;
    opacity: var(--stop);
  }
  .run-button.running {
    color: var(--err);
  }
  .run-button.pending,
  .run-button[aria-disabled="true"] {
    filter: saturate(0.7);
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
