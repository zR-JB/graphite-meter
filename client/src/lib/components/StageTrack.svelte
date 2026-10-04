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
  import {
    STAGE,
    STATUS,
    STATUS_TONE,
    type Tone,
  } from "../presentation/vocabulary";
  import { fmtDuration, formatLatency, resultRate } from "../format";
  import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
  import type { StageKey } from "../state/store.svelte";
  import { planned, STAGES } from "../runner/schedule";
  import { handoff, type Handoff } from "../presentation/motion.svelte";

  const controller = getApplicationController();

  // A stage's result as a figure and its unit; the latency stage's keeps its unit with the figure.
  function resultFigure(key: StageKey): { num: string; unit: string } | null {
    if (key === "latency") {
      const latency = store.stageResults.latency;
      return latency
        ? { num: formatLatency(latency.reportedMs), unit: "" }
        : null;
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
      : resultRate(bytes, {
          base: store.unitBase,
          kind: store.unitKind,
          tier: store.scales.unitIndex,
        });
  }
  const resultValue = (key: StageKey) => {
    const figure = resultFigure(key);
    return figure ? `${figure.num} ${figure.unit}`.trim() : null;
  };

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
  // A key's end: the stage's result once measured, the time into the stage while it runs, its state word while
  // it waits. The card under the key draws the stage's progress, so the key's bar shows only how it settled.
  const look = (key: StageKey) => {
    const s = model.find((s) => s.key === key)!;
    const live = s.state === "active" && store.phaseBudgetMs > 0;
    const done = s.state === "complete" || s.state === "partial";
    const figure = done ? resultFigure(key) : null;
    return {
      state: s.state,
      live,
      text: figure
        ? figure.num
        : live
          ? `${fmtDuration(store.phaseClock.current, 1)} / ${fmtDuration(store.phaseBudgetMs, 0)}`
          : s.reason,
      unit: figure?.unit ?? "",
      tone: s.state === "recovering" ? STATUS_TONE.recovering : key,
      progress:
        s.state === "warmup" || s.state === "failed"
          ? 1
          : live
            ? 0
            : s.fill / 100,
    };
  };
  // A result or word fades in with its state; the running time counts in place.
  const looks = Object.fromEntries(
    STAGES.map((key) => [
      key,
      handoff(
        () => look(key),
        (shown) =>
          shown.live
            ? `${shown.state}:live`
            : `${shown.state}:${shown.text}:${shown.unit}`,
      ),
    ]),
  ) as Record<StageKey, Handoff<ReturnType<typeof look>>>;
</script>

<div class="stage-track" role="group" aria-label="Test stages">
  {#each segments as s (s.key)}
    {@const view = looks[s.key]}
    {@const look = view.shown}
    <button
      type="button"
      class="chip chip--{s.state}"
      class:on={s.selected}
      class:done={look.state === "complete"}
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
      <span class="chip-key" aria-hidden="true">
        {#if look.state === "complete"}<Icon name="check" />{/if}
      </span>
      <span class="chip-label">{s.label}</span>
      {#if look.text}
        <span
          class="chip-res"
          class:value={look.state === "complete" || look.state === "partial"}
          data-tone={look.live
            ? undefined
            : (STATUS_TONE as Record<string, Tone>)[look.state]}
          style:opacity={view.opacity}
          >{look.text}{#if look.unit}<span class="chip-unit">{look.unit}</span
            >{/if}</span
        >
      {/if}
      <span class="chip-bar" aria-hidden="true">
        <span
          class="chip-fill"
          data-tone={look.tone}
          class:chip-fill--warmup={look.state === "warmup"}
          class:chip-fill--failed={look.state === "failed"}
          class:is-partial={look.state === "partial"}
          class:is-stalled={look.state === "recovering"}
          class:is-live={look.live}
          style:--progress={look.progress}
        ></span>
      </span>
    </button>
  {/each}
</div>

<style>
  /* The keys take the cards' columns, so each stands over its stage's card; the row lays them out. */
  .stage-track {
    display: contents;
  }

  /* A key: a flat plate with a check box, the stage's name and, at its end, its result; along its base a bar in
     the stage's hue once the stage has settled. */
  .chip {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 46px;
    min-width: 0;
    padding: 0 12px;
    overflow: hidden;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    color: var(--text);
    text-align: start;
    transition: var(--transition-control);
  }
  @media (hover: hover) {
    .chip:hover:not(:disabled) {
      border-color: var(--border-strong);
    }
  }
  /* One off stays operable, so it reads soft rather than dimmed; only a locked key dims. */
  .chip:not(.on) {
    color: var(--text-soft);
  }
  .chip:disabled {
    cursor: default;
  }
  .chip--disabled:disabled {
    opacity: 0.5;
  }
  /* The box is the switch: ink square while on, the hue's check once measured. */
  .chip-key {
    display: grid;
    flex: none;
    place-items: center;
    width: 13px;
    height: 13px;
    border: 1px solid var(--field-edge);
    border-radius: 1px;
    color: var(--tone);
  }
  .on .chip-key {
    border-color: var(--brand);
  }
  .on .chip-key::after {
    content: "";
    width: 7px;
    height: 7px;
    background: var(--brand);
  }
  .done .chip-key {
    border-color: var(--tone);
  }
  .done .chip-key::after {
    display: none;
  }
  .chip-key :global(svg) {
    width: 10px;
    height: 10px;
  }
  .chip-label {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    font: 500 var(--type-body) / 1 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-res {
    flex: none;
    color: var(--text-muted);
    font: var(--role-figure-sm);
    line-height: 1;
    white-space: nowrap;
  }
  .chip-res.value {
    color: var(--text);
  }
  .chip-res[data-tone] {
    color: var(--tone-ink);
  }
  .chip-unit {
    margin-left: 0.5ch;
    color: var(--text-muted);
  }
  /* A narrower row keeps the figure and drops its unit, which the dial names. */
  @container viz (max-width: 1180px) {
    .chip-unit {
      display: none;
    }
  }
  .chip-bar {
    position: absolute;
    inset: auto 0 0;
    height: 2px;
    overflow: hidden;
  }
  .chip-fill {
    position: absolute;
    inset: 0;
    background: var(--tone);
    transform: scaleX(var(--progress, 0));
    transform-origin: left center;
    transition:
      transform var(--dur-graph) var(--ease-out),
      background-color var(--dur-graph) var(--ease-out);
  }
  .chip-fill.is-live {
    transition: background-color var(--dur-graph) var(--ease-out);
  }
  /* A finished stage keeps its hue; partial is hatched like the dial's dashed arc. */
  .chip-fill.is-partial {
    background: repeating-linear-gradient(
      90deg,
      var(--tone) 0 6px,
      transparent 6px 9px
    );
  }
  .chip-fill--failed {
    background: var(--err);
    opacity: 0.45;
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
  @container viz (max-width: 720px) {
    .chip {
      padding: 0 10px;
    }
  }
  @container viz (max-width: 430px) {
    .chip {
      display: grid;
      grid-template-columns: auto minmax(0, 1fr);
      grid-template-rows: auto auto;
      row-gap: 2px;
      column-gap: 8px;
      align-content: center;
    }
    .chip-key {
      grid-row: 1 / 3;
    }
    .chip-res {
      grid-column: 2;
      justify-self: start;
      font-size: var(--type-xs);
    }
  }
</style>
