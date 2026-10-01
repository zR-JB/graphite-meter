<script lang="ts">
  import Icon from "./Icon.svelte";
  // Editable selection stays separate from the retained run's execution.
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { tooltipAction } from "../actions/tooltip";
  import {
    lockReason,
    stageShown,
    stageTip,
    stageTrackModel,
  } from "./stageTrack";
  import { STAGE, STATUS, STATUS_TONE } from "../presentation/vocabulary";
  import { formatLatency, formatRate } from "../format";
  import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
  import type { StageKey } from "../state/store.svelte";
  import { planned, STAGES } from "../runner/schedule";

  const controller = getApplicationController();

  function resultValue(key: StageKey): string | null {
    if (key === "latency") {
      const latency = store.stageResults.latency;
      return latency ? formatLatency(latency.reportedMs) : null;
    }
    const bytes =
      key === "bidirectional"
        ? bidirectionalResultPresentation(
            store.result?.bidirectional?.down?.reportedBytesPerSec,
            store.result?.bidirectional?.up?.reportedBytesPerSec,
          ).combinedBytesPerSec
        : store.stageResults[key]?.reportedBytesPerSec;
    return bytes == null
      ? null
      : formatRate(bytes, {
          base: store.unitBase,
          kind: store.unitKind,
          tier: store.scales.unitIndex,
        });
  }

  const model = $derived(
    STAGES.map((key) => {
      const execution = store.stagePresentation[key];
      // The run skips a stage without a duration, so the chip does too.
      const selected = planned(store.config, key);
      const locked = !store.canToggleStage(key);
      const model = stageTrackModel({ selected, locked, execution });
      const reason =
        model.tag ??
        lockReason(!locked, store.phase, store.phaseStage, key, model.state);
      return {
        ...model,
        key,
        label: STAGE[key].short,
        icon: STAGE[key].icon,
        reason,
        tip: stageTip({
          selected,
          locked,
          state: model.state,
          reason,
          failure: execution.failure,
          value: resultValue(key),
        }),
      };
    }),
  );
  const segments = $derived(
    model.filter((s) =>
      stageShown(s.key, s.selected, store.stagePresentation[s.key]),
    ),
  );
  // The line under the name: the stage's progress while it runs, how it settled after.
  const line = (s: (typeof model)[number]) => {
    const live = s.state === "active" && store.phaseBudgetMs > 0;
    return {
      tone: s.state === "recovering" ? STATUS_TONE.recovering : s.key,
      live,
      progress:
        s.state === "warmup" || s.state === "failed"
          ? 1
          : live
            ? store.phaseClock.current / store.phaseBudgetMs
            : s.fill / 100,
    };
  };
</script>

<fieldset class="stage-track">
  <legend class="sr-only">Test stages, toggle to include or skip</legend>
  {#each segments as s (s.key)}
    {@const look = line(s)}
    <button
      type="button"
      class="chip chip--{s.state}"
      class:btn={!s.locked || s.state === "pending"}
      class:on={s.selected}
      data-tone={s.key}
      role="switch"
      aria-checked={s.selected}
      aria-label="{s.label} stage{s.reason
        ? ` (${s.reason})`
        : s.state === 'complete'
          ? ` (${STATUS.complete})`
          : ''}"
      use:tooltipAction={s.tip}
      disabled={s.locked}
      onclick={() => controller.toggleStage(s.key)}
    >
      <!-- One glyph per chip, so no state changes its width and nothing on the row moves. -->
      <span class="bead" aria-hidden="true"><Icon name="check" /></span>
      <span class="chip-label">{s.label}</span>
      <span class="chip-bar" aria-hidden="true">
        <span
          class="chip-fill"
          data-tone={look.tone}
          class:chip-fill--warmup={s.state === "warmup"}
          class:chip-fill--failed={s.state === "failed"}
          class:is-partial={s.state === "partial"}
          class:is-stalled={s.state === "recovering"}
          class:is-live={look.live}
          style:--progress={look.progress}
        ></span>
      </span>
    </button>
  {/each}
</fieldset>

<style>
  .stage-track {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--space-2);
  }
  /* A chip that can change is a switch drawn as an ink control (.btn); locked by a run, it is the run's progress.
     Its size is its own, so locking never moves the row. */
  .chip {
    --hit-pad: 0px;
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    height: 36px;
    padding: 0 var(--space-3) 0 10px;
    border-radius: var(--r-chrome);
    color: var(--text-soft);
    font: var(--w-normal) var(--type-body) / 1 var(--font-sans);
    white-space: nowrap;
    transition: var(--transition-control);
  }
  /* A finger's target reaches past the plate to a full hit height. */
  @media (pointer: coarse) {
    .chip::before {
      position: absolute;
      inset: calc((100% - var(--hit)) / 2) 0;
      content: "";
    }
  }
  /* The washes are layers, so they add to the plate instead of replacing it. */
  @media (hover: hover) {
    .chip:hover:not(:disabled) {
      background-image: linear-gradient(var(--hover-wash) 0 0);
      color: var(--text);
    }
  }
  .chip:active:not(:disabled) {
    background-image: linear-gradient(var(--selected-wash) 0 0);
  }
  .chip.on {
    color: var(--text-muted);
  }
  .chip--active,
  .chip--warmup,
  .chip--recovering {
    background: color-mix(in oklab, var(--tone) 14%, transparent);
    color: var(--text);
  }
  .chip:disabled {
    cursor: default;
  }
  /* Locked only while the test starts, a chip the run has not reached keeps its plate, undimmed, so nothing blinks. */
  .chip.btn:disabled {
    opacity: 1;
  }
  .chip--disabled:disabled {
    opacity: 0.5;
  }
  /* The bead is the stage's colour: filled when it runs, a ring when it is off, a check once complete. */
  .bead {
    position: relative;
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: var(--r-full);
    box-shadow: inset 0 0 0 1.5px var(--tone);
    transition: background-color var(--dur-graph) var(--ease-out);
  }
  .on .bead {
    background: var(--tone);
    box-shadow: none;
  }
  /* Centred on the bead, the check crossfades with it. */
  .bead :global(svg) {
    position: absolute;
    top: calc((8px - var(--icon-sm)) / 2);
    left: calc((8px - var(--icon-sm)) / 2);
    width: var(--icon-sm);
    height: var(--icon-sm);
    color: var(--tone-ink);
    opacity: 0;
    transition: opacity var(--dur-graph) var(--ease-out);
  }
  .chip--complete .bead {
    background-color: transparent;
  }
  .chip--complete .bead :global(svg) {
    opacity: 1;
  }
  .chip-label {
    font-weight: var(--w-strong);
  }
  /* Progress is a hairline under the label, like a stage's rule. */
  .chip-bar {
    position: absolute;
    inset: auto var(--space-3) 5px 10px;
    height: 2px;
    overflow: hidden;
    border-radius: var(--r-full);
  }
  .chip-fill {
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: var(--tone);
    transform: scaleX(var(--progress, 0));
    transform-origin: left center;
    transition:
      transform var(--dur-graph) var(--ease-out),
      background-color var(--dur-graph) var(--ease-out);
  }
  .chip--complete .chip-fill {
    opacity: 0;
  }
  .chip-fill.is-live {
    transition: background-color var(--dur-graph) var(--ease-out);
  }
  .chip-fill.is-partial {
    background: repeating-linear-gradient(
      90deg,
      var(--tone) 0 6px,
      transparent 6px 9px
    );
  }
  .chip-fill--failed {
    background: var(--err);
  }
  .chip-fill.is-stalled {
    animation: stall-pulse var(--dur-pulse) var(--ease-out) infinite;
  }
  @keyframes stall-pulse {
    50% {
      opacity: 0.4;
    }
  }
  .chip-fill--warmup {
    width: 45%;
    background: color-mix(in oklab, var(--tone) 55%, transparent);
    animation: warmup-sweep var(--dur-pulse) var(--ease-out) infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .chip-fill--warmup {
      width: 100%;
      opacity: 0.55;
    }
  }
  @keyframes warmup-sweep {
    from {
      translate: -110%;
    }
    to {
      translate: 240%;
    }
  }
  /* A phone gives the chips one row of equal columns; a chip's name never widens its column. */
  @container viz (max-width: 520px) {
    .stage-track {
      display: grid;
      grid-auto-columns: minmax(0, 1fr);
      grid-auto-flow: column;
    }
    .chip {
      gap: var(--space-1);
      padding: 0;
      font-size: var(--type-sm);
    }
    .chip-bar {
      inset-inline: var(--space-2);
    }
  }
  /* A 320 px phone's 66 px columns hold the names at 11 px. */
  @container viz (max-width: 300px) {
    .chip {
      font-size: var(--type-xs);
    }
  }
</style>
