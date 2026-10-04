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
      // The run skips a stage without a duration, so the key does too.
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
        label: STAGE[key].short,
        icon: STAGE[key].icon,
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
  // The key's second line: the result once measured, the time while it runs, its plan before, a word otherwise.
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
  {#each segments as s (s.key)}
    {@const look = line(s)}
    {@const second = detail(s, look.live)}
    <button
      type="button"
      class="key key--{s.state}"
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
      <span class="key-head">
        <!-- One glyph per key, so no state changes its width and nothing on the row moves. -->
        <span class="bead" aria-hidden="true"><Icon name="check" /></span>
        <span class="key-label">{s.label}</span>
      </span>
      <span class="key-line" class:figure={second.figure} aria-hidden="true"
        >{second.text}</span
      >
      <span class="key-bar" aria-hidden="true">
        <span
          class="key-fill"
          data-tone={look.tone}
          class:key-fill--warmup={s.state === "warmup"}
          class:key-fill--failed={s.state === "failed"}
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
  /* One key per stage, in equal columns, so the row keeps its shape in every state. */
  .stage-track {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(0, 1fr);
    gap: var(--space-3);
    min-width: 0;
  }
  /* A key is a small frame: its name over its figure, its stage's line along its base. Locked by a run it
     shows the run's progress; off, it is its outline alone. */
  .key {
    position: relative;
    display: grid;
    align-content: start;
    gap: 3px;
    min-width: 0;
    min-height: 54px;
    padding: var(--space-2) var(--space-3) 10px;
    overflow: hidden;
    border: var(--hairline) solid var(--panel-edge);
    border-radius: var(--r-surface);
    background: var(--panel);
    box-shadow: var(--elev-card);
    color: var(--text);
    text-align: start;
    transition: var(--transition-control);
  }
  /* The washes are layers, so they add to the frame instead of replacing it. */
  @media (hover: hover) {
    .key:hover:not(:disabled) {
      background-image: linear-gradient(var(--hover-wash) 0 0);
    }
  }
  .key:active:not(:disabled) {
    background-image: linear-gradient(var(--selected-wash) 0 0);
  }
  .key:not(.on) {
    background-color: transparent;
    box-shadow: none;
    color: var(--text-muted);
  }
  /* The running key takes its stage's light. */
  .key--active,
  .key--warmup,
  .key--recovering {
    background-color: color-mix(in oklab, var(--tone) 12%, var(--panel));
  }
  .key:disabled {
    cursor: default;
  }
  .key--disabled:disabled {
    opacity: 0.5;
  }
  .key-head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }
  /* The bead is the stage's key: filled when it runs, an outline when it is off, a check once complete. */
  .bead {
    position: relative;
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 1px;
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
  .key--complete .bead {
    background-color: transparent;
  }
  .key--complete .bead :global(svg) {
    opacity: 1;
  }
  .key-label {
    overflow: hidden;
    font: var(--w-strong) var(--type-body) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .key-line {
    overflow: hidden;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .key-line.figure {
    color: var(--text-muted);
    font: var(--role-figure-sm);
    font-variant-numeric: tabular-nums;
  }
  /* Progress is the stage's line along the key's base. */
  .key-bar {
    position: absolute;
    inset: auto 0 0 0;
    height: 3px;
    overflow: hidden;
  }
  .key-fill {
    position: absolute;
    inset: 0;
    background: var(--tone);
    transform: scaleX(var(--progress, 0));
    transform-origin: left center;
    transition:
      transform var(--dur-graph) var(--ease-out),
      background-color var(--dur-graph) var(--ease-out);
  }
  .key-fill.is-live {
    transition: background-color var(--dur-graph) var(--ease-out);
  }
  .key-fill.is-partial {
    background: repeating-linear-gradient(
      90deg,
      var(--tone) 0 6px,
      transparent 6px 9px
    );
  }
  .key-fill--failed {
    background: var(--err);
  }
  .key-fill.is-stalled {
    animation: stall-pulse var(--dur-pulse) var(--ease-out) infinite;
  }
  @keyframes stall-pulse {
    50% {
      opacity: 0.4;
    }
  }
  .key-fill--warmup {
    width: 45%;
    background: color-mix(in oklab, var(--tone) 55%, transparent);
    animation: warmup-sweep var(--dur-pulse) var(--ease-out) infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .key-fill--warmup {
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
  /* A phone gives the keys two columns. */
  @container viz (max-width: 520px) {
    .stage-track {
      grid-auto-flow: row;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: var(--space-2);
    }
    .key {
      padding-inline: 10px;
    }
  }
</style>
