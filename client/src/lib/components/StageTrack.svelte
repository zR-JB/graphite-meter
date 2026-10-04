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
  import { fmtDuration, formatLatency, formatRate } from "../format";
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
      // The run skips a stage without a duration, so its row does too.
      const selected = planned(store.config, key);
      const locked = !store.canToggleStage(key);
      const model = stageTrackModel({ selected, locked, execution });
      const reason =
        model.tag ??
        lockReason(!locked, store.phase, store.phaseStage, key, model.state);
      const value = resultValue(key);
      return {
        ...model,
        key,
        label: STAGE[key].label,
        reason,
        value,
        tip: stageTip({
          selected,
          locked,
          state: model.state,
          reason,
          failure: execution.failure,
          value,
        }),
      };
    }),
  );
  const rows = $derived(
    model.filter((s) =>
      stageShown(s.key, s.selected, store.stagePresentation[s.key]),
    ),
  );
  // The line across the row: the stage's progress while it runs, how it settled after.
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
  // The row's figure: the result once measured, the time while it runs, its plan before, a word otherwise.
  const detail = (s: (typeof model)[number], live: boolean) => {
    if (s.value && (s.state === "complete" || s.state === "partial"))
      return { text: s.value, figure: true };
    if (live)
      return {
        text: `${fmtDuration(store.phaseClock.current, 0)} / ${fmtDuration(store.phaseBudgetMs, 0)}`,
        figure: true,
      };
    if (s.state === "pending" && s.selected)
      return {
        text: fmtDuration(store.config.duration[`${s.key}Ms`], 0),
        figure: true,
      };
    if (!s.selected) return { text: STATUS["not-run"], figure: false };
    return {
      text:
        s.state === "active"
          ? STATUS.running
          : (STATUS[s.state as keyof typeof STATUS] ?? ""),
      figure: false,
    };
  };
</script>

<fieldset class="stage-track">
  <legend class="sr-only">Test stages, toggle to include or skip</legend>
  {#each rows as s (s.key)}
    {@const look = line(s)}
    {@const second = detail(s, look.live)}
    <button
      type="button"
      class="row row--{s.state}"
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
      <span class="check" aria-hidden="true"><Icon name="check" /></span>
      <span class="name">{s.label}</span>
      <span class="track" aria-hidden="true">
        <span
          class="fill"
          data-tone={look.tone}
          class:fill--warmup={s.state === "warmup"}
          class:fill--failed={s.state === "failed"}
          class:is-partial={s.state === "partial"}
          class:is-stalled={s.state === "recovering"}
          class:is-live={look.live}
          style:--progress={look.progress}
        ></span>
      </span>
      <span class="figure" class:word={!second.figure} aria-hidden="true"
        >{second.text}</span
      >
    </button>
  {/each}
</fieldset>

<style>
  /* The run sheet: one row per stage, each a switch. A row is its check, its name, its line and its figure. */
  .stage-track {
    display: grid;
    min-width: 0;
  }
  .row {
    display: grid;
    grid-template-columns: var(--check) 7.5rem minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
    min-height: 44px;
    padding: 0 var(--space-2);
    margin-inline: calc(-1 * var(--space-2));
    border-radius: var(--r-chrome);
    color: var(--text);
    text-align: start;
    transition: var(--transition-control);
  }
  .row + .row {
    margin-top: 2px;
  }
  @media (hover: hover) {
    .row:hover:not(:disabled) {
      background: var(--hover-wash);
    }
  }
  .row:active:not(:disabled) {
    background: var(--selected-wash);
  }
  .row:disabled {
    cursor: default;
  }
  .row--disabled:disabled {
    opacity: 0.5;
  }
  .row:not(.on) {
    color: var(--text-soft);
  }
  /* The check is the switch, drawn like every check: an edge when off, ink with the mark when on. */
  .check {
    display: grid;
    place-items: center;
    width: var(--check);
    height: var(--check);
    border: var(--check-edge);
    border-radius: var(--r-well);
    background: var(--surface-1);
    color: var(--text-inverse);
    transition: var(--transition-control);
  }
  .check :global(svg) {
    width: var(--icon-sm);
    height: var(--icon-sm);
    opacity: 0;
    transition: opacity var(--dur-slide) var(--ease-out);
  }
  .on .check {
    border-color: var(--brand);
    background: var(--brand);
  }
  .on .check :global(svg) {
    opacity: 1;
  }
  .name {
    overflow: hidden;
    font: var(--w-strong) var(--type-body) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The line is the stage's progress on its own track; a stage not in the run has no line. */
  .track {
    position: relative;
    height: 4px;
    overflow: hidden;
    border-radius: 2px;
    background: var(--track);
  }
  .row:not(.on) .track {
    visibility: hidden;
  }
  .fill {
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
  .fill.is-live {
    transition: background-color var(--dur-graph) var(--ease-out);
  }
  .fill.is-partial {
    background: repeating-linear-gradient(
      90deg,
      var(--tone) 0 6px,
      transparent 6px 9px
    );
  }
  .fill--failed {
    background: var(--err);
  }
  .fill.is-stalled {
    animation: stall-pulse var(--dur-pulse) var(--ease-out) infinite;
  }
  @keyframes stall-pulse {
    50% {
      opacity: 0.4;
    }
  }
  .fill--warmup {
    width: 45%;
    background: color-mix(in oklab, var(--tone) 55%, transparent);
    animation: warmup-sweep var(--dur-pulse) var(--ease-out) infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .fill--warmup {
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
  /* Figures are as wide as their longest value, so the lines end on one edge. */
  .figure {
    min-width: 8ch;
    font: var(--role-figure-sm);
    font-variant-numeric: tabular-nums;
    text-align: end;
    white-space: nowrap;
  }
  .figure.word {
    color: var(--text-soft);
    font: var(--role-label);
  }
  @container viz (max-width: 520px) {
    .row {
      grid-template-columns: var(--check) 6rem minmax(0, 1fr) auto;
      gap: var(--space-2);
    }
  }
</style>
