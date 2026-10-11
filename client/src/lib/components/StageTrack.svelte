<script lang="ts">
  import Icon from "./Icon.svelte";
  // Editable selection stays separate from the retained run's execution.
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { clipTip, tooltipAction } from "../actions/tooltip";
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
  import { fmtDuration, formatLatency, formatRate } from "../format";
  import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
  import type { StageKey } from "../state/store.svelte";
  import { planned, STAGES } from "../runner/schedule";
  import { handoff, type Handoff } from "../presentation/motion.svelte";
  import type { Snippet } from "svelte";

  /** The run key, set over the chips as wide as their row. */
  let { children }: { children?: Snippet } = $props();

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
  const plannedMs = (key: StageKey) =>
    (store.run?.config ?? store.config).duration[`${key}Ms`];
  // The bar along a chip's top is the stage's progress while it runs and how it settled after; the chip's end is
  // the time into the stage while it runs, the stage's length while it waits, and its state word otherwise.
  const look = (key: StageKey) => {
    const s = model.find((s) => s.key === key)!;
    const live = s.state === "active" && store.phaseBudgetMs > 0;
    const waiting =
      s.selected &&
      (s.state === "pending" ||
        (s.state === "disabled" && !store.isRunning && !s.tag));
    const upcoming = s.selected && s.tag === STATUS.next;
    const time = live || waiting || upcoming;
    return {
      state: s.state,
      live,
      time,
      text: live
        ? `${(store.phaseClock.current / 1000).toFixed(1)} / ${fmtDuration(store.phaseBudgetMs, 0)}`
        : time
          ? fmtDuration(plannedMs(key), 0)
          : s.reason,
      // A narrow chip keeps the time into the stage and drops its length.
      short: live ? fmtDuration(store.phaseClock.current, 1) : null,
      tone: s.state === "recovering" ? STATUS_TONE.recovering : key,
      tagTone: time
        ? undefined
        : (STATUS_TONE as Record<string, Tone | undefined>)[s.state],
      progress:
        s.state === "warmup" || s.state === "failed"
          ? 1
          : live
            ? store.phaseClock.current / store.phaseBudgetMs
            : s.fill / 100,
    };
  };
  // A word or check fades in with its state; the running time counts in place.
  const looks = Object.fromEntries(
    STAGES.map((key) => [
      key,
      handoff(
        () => look(key),
        (shown) =>
          shown.live ? `${shown.state}:live` : `${shown.state}:${shown.text}`,
      ),
    ]),
  ) as Record<StageKey, Handoff<ReturnType<typeof look>>>;
</script>

<div
  class="stage-track"
  style:--chips={segments.length}
  style:--cols={segments.length > 3 ? 2 : segments.length}
>
  {@render children?.()}
  <div class="chips" role="group" aria-label="Test stages">
    <span class="legend caps" aria-hidden="true">Test stages</span>
    {#each segments as s (s.key)}
      {@const view = looks[s.key]}
      {@const look = view.shown}
      <button
        type="button"
        class="chip chip--{s.state}"
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
        <span class="chip-row">
          <span class="chip-ico" aria-hidden="true"><Icon name={s.icon} /></span
          >
          <span class="chip-label" use:clipTip>{s.label}</span>
          {#if look.state === "complete"}
            <span class="chip-check handoff" class:handoff-out={view.out}
              ><Icon name="check" /></span
            >
          {:else if look.text}
            <span
              class="chip-tag handoff"
              class:time={look.time}
              class:handoff-out={view.out}
              data-tone={look.tagTone}
              >{#if look.short}<span class="full">{look.text}</span><span
                  class="short">{look.short}</span
                >{:else}{look.text}{/if}</span
            >
          {/if}
        </span>
      </button>
    {/each}
  </div>
</div>

<style>
  /* The key over the chips, both as wide as the chip row: as many chips to a line as fit, 140–172 px each,
     with the caption on the first chip's edge. */
  .stage-track {
    container: track / inline-size;
    display: grid;
    gap: var(--space-3);
    width: min(
      100%,
      calc(var(--chips) * 172px + (var(--chips) - 1) * var(--space-2))
    );
  }
  .chips {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 6px var(--space-2);
  }
  .legend {
    grid-column: 1 / -1;
  }
  /* Four chips that cannot stand in one row stand two and two, never three and one. */
  @container track (max-width: 583px) {
    .chips:has(> :nth-child(5)) {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  /* A chip: its stage's bar along the top, then its glyph, name and, at the end, its time, word or check. */
  .chip {
    display: grid;
    container-type: inline-size;
    gap: 7px;
    align-content: center;
    min-width: 0;
    height: 46px;
    padding: 8px 10px;
    overflow: hidden;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
    color: var(--text);
    text-align: start;
    transition: var(--transition-control);
  }
  @media (hover: hover) {
    .chip:hover:not(:disabled) {
      border-color: var(--border-strong);
    }
  }
  /* One off stays operable, so it reads soft rather than dimmed; only a locked chip dims. */
  .chip:not(.on) {
    border-color: var(--border-subtle);
    color: var(--text-soft);
  }
  .chip:disabled {
    cursor: default;
  }
  .chip--disabled:disabled {
    opacity: 0.5;
  }
  /* The running chip takes its hue as its edge, like the running card. */
  .chip--active,
  .chip--warmup,
  .chip--recovering {
    border-color: color-mix(in oklab, var(--tone) 55%, var(--border));
  }
  .chip-bar {
    position: relative;
    height: 3px;
    overflow: hidden;
    border-radius: var(--r-full);
    background: var(--surface-inset);
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
  .chip-fill.is-live {
    transition: background-color var(--dur-graph) var(--ease-out);
  }
  /* A finished stage's check pops in on the spring. A press gives inside the chip's border, so the hit box
     never moves under the finger. */
  .chip > * {
    transition: scale var(--dur-graph) var(--ease-spring);
  }
  @media (prefers-reduced-motion: no-preference) {
    .chip-check {
      animation: pop 380ms var(--ease-spring) backwards;
    }
    .chip:active:not(:disabled) > * {
      scale: 0.96;
      transition-duration: var(--dur-hover);
    }
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
  .chip-row {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 16px;
    min-width: 0;
  }
  .chip-ico {
    display: grid;
    flex: none;
    place-items: center;
  }
  .chip-ico :global(svg) {
    width: 14px;
    height: 14px;
  }
  /* The clip box reaches past a one-em line by a descender's depth, which Firefox would cut. */
  .chip-label {
    flex: 1 1 auto;
    min-width: 0;
    margin-block: -0.2em;
    padding-block: 0.2em;
    overflow: hidden;
    font: var(--w-heavy) var(--type-sm) / 1 var(--font-sans);
    letter-spacing: var(--track-tight);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip-check {
    display: grid;
    flex: none;
    color: var(--ok);
  }
  .chip-check :global(svg) {
    width: 12px;
    height: 12px;
  }
  /* A state word is a small engraved tag; a time is a plain figure. */
  .chip-tag {
    flex: none;
    padding: 2px 5px;
    border-radius: var(--r-well);
    background: var(--surface-inset);
    color: var(--text-soft);
    font: var(--w-strong) var(--type-2xs) / 1 var(--font-mono);
    letter-spacing: var(--track-caps);
    text-transform: uppercase;
    white-space: nowrap;
  }
  .chip-tag[data-tone] {
    color: var(--tone-ink);
  }
  .chip-tag.time {
    padding: 0;
    background: none;
    color: var(--text-muted);
    font: var(--role-figure-sm);
    line-height: 1;
    letter-spacing: 0;
    text-transform: none;
  }
  .chip-tag .short {
    display: none;
  }
  /* A chip without room for the time into the stage and its length keeps the time alone. */
  @container (max-width: 172px) {
    .chip-tag .full {
      display: none;
    }
    .chip-tag .short {
      display: inline;
    }
  }
  /* A phone keeps up to three chips on one line and sets four as two and two. */
  :global(.gauge-panel.tight) {
    .chips {
      grid-template-columns: repeat(var(--cols), minmax(0, 1fr));
    }
    /* The chips under the key name themselves; the legend's line goes to the cards. */
    .legend {
      display: none;
    }
    .chip {
      gap: 5px;
      padding: 6px 8px;
    }
  }
  /* A chip too narrow for its glyph sets its name over its time, word or check, so neither is cut. */
  @container (max-width: 124px) {
    .chip-ico {
      display: none;
    }
    .chip-row {
      flex-wrap: wrap;
      row-gap: 3px;
      height: auto;
    }
    .chip-label {
      flex-basis: 100%;
      font-size: var(--type-xs);
    }
    .chip-tag.time {
      font-size: 10px;
    }
    .chip-tag:not(.time) {
      padding: 1px 3px;
      font-size: 9px;
    }
  }
</style>
