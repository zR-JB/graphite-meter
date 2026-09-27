<script lang="ts">
  import Icon from "./Icon.svelte";
  // Editable selection stays separate from the retained run's execution.
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { tooltip } from "../actions/tooltip";
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
  import { formatLatency, formatRate } from "../format";
  import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
  import type { StageKey } from "../state/store.svelte";
  import { planned, STAGES } from "../runner/schedule";
  import { handoff, type Handoff } from "../presentation/motion.svelte";

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
      : formatRate(bytes, { base: store.unitBase, kind: store.unitKind });
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
  const look = (key: StageKey) => {
    const s = model.find((s) => s.key === key)!;
    const live = s.state === "active" && store.phaseBudgetMs > 0;
    return {
      state: s.state,
      reason: s.reason,
      tone: s.state === "recovering" ? STATUS_TONE.recovering : key,
      live,
      progress:
        s.state === "warmup" || s.state === "failed"
          ? 1
          : live
            ? store.phaseClock.current / store.phaseBudgetMs
            : s.fill / 100,
    };
  };
  const looks = Object.fromEntries(
    STAGES.map((key) => [
      key,
      handoff(
        () => look(key),
        (shown) => `${shown.state}:${shown.reason}`,
      ),
    ]),
  ) as Record<StageKey, Handoff<ReturnType<typeof look>>>;
  const SETTLED = new Set(["partial", "failed", "stopped"]);
</script>

<fieldset class="stage-track" class:quad={segments.length === 4}>
  <legend
    >Test stages<span class="sr-only">
      — toggle to include or skip</span
    ></legend
  >
  {#each segments as s (s.key)}
    {@const view = looks[s.key]}
    {@const look = view.shown}
    <button
      type="button"
      class="seg seg--{s.state}"
      class:on={s.selected}
      role="switch"
      aria-checked={s.selected}
      aria-label="{s.label} stage{s.reason
        ? ` (${s.reason})`
        : s.state === 'complete'
          ? ` (${STATUS.complete})`
          : ''}"
      {@attach tooltip(() => s.tip)}
      disabled={s.locked}
      onclick={() => controller.toggleStage(s.key)}
    >
      <div class="seg-bar" aria-hidden="true">
        <span
          class="seg-fill"
          data-tone={look.tone}
          class:seg-fill--warmup={look.state === "warmup"}
          class:seg-fill--failed={look.state === "failed"}
          class:is-partial={look.state === "partial"}
          class:is-stalled={look.state === "recovering"}
          class:is-live={look.live}
          style:--progress={look.progress}
        ></span>
      </div>
      <span class="seg-row">
        <span class="seg-main">
          <span class="seg-ico"><Icon name={s.icon} /></span>
          <span class="seg-label">{s.label}</span>
        </span>
        <!-- A settled result's word lives on its chip below; the bar's pattern keeps the state here. -->
        {#if look.reason && !SETTLED.has(look.state)}
          <span
            class="seg-tag"
            data-tone={(STATUS_TONE as Record<string, Tone>)[look.state]}
            style:opacity={view.opacity}>{look.reason}</span
          >
        {:else if look.state === "complete"}
          <span class="seg-ico seg-check" style:opacity={view.opacity}
            ><Icon name="check" /></span
          >
        {/if}
      </span>
    </button>
  {/each}
</fieldset>

<style>
  .stage-track {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: var(--space-2);
  }
  .stage-track.quad {
    grid-template-columns: repeat(4, minmax(0, 1fr));
  }
  legend {
    margin-bottom: var(--space-2);
    color: var(--text-soft);
    font: var(--w-strong) var(--type-xs) / 1.2 var(--font-sans);
  }
  @container (max-width: 430px) {
    .stage-track.quad {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  .seg {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    height: 46px;
    padding: var(--space-2);
    overflow: hidden;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: var(--elev-tile);
    color: var(--text-muted);
    text-align: start;
    transition:
      var(--transition-control),
      transform var(--dur-hover) var(--ease-out);
  }
  @media (hover: hover) {
    .seg:hover:not(:disabled) {
      border-color: var(--border-strong);
      color: var(--text);
      transform: translateY(-1px);
    }
  }
  .seg:active:not(:disabled) {
    transform: none;
  }
  .seg.on {
    border-color: var(--brand-line);
    background: var(--brand-soft);
    color: var(--text);
  }
  /* A deselected stage stays operable, so it reads soft rather than dimmed; only a locked one dims. */
  .seg--disabled {
    color: var(--text-soft);
  }
  .seg:disabled {
    opacity: 0.5;
  }

  .seg-bar {
    position: relative;
    flex: none;
    height: 5px;
    overflow: hidden;
    border-radius: var(--r-full);
    background: var(--surface-inset);
  }
  .seg-fill {
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
  .seg-fill.is-live {
    transition: background-color var(--dur-graph) var(--ease-out);
  }
  /* A finished stage keeps its phase tone; partial is hatched like the gauge's dashed arc. */
  .seg-fill.is-partial {
    background: repeating-linear-gradient(
      90deg,
      var(--tone) 0 6px,
      transparent 6px 9px
    );
  }
  .seg-fill--failed {
    background: var(--err);
    opacity: 0.45;
  }
  .seg-fill.is-stalled {
    animation: stall-pulse var(--dur-pulse) var(--ease-out) infinite;
  }
  @keyframes stall-pulse {
    50% {
      opacity: 0.4;
    }
  }
  .seg-fill--warmup {
    width: 45%;
    background: color-mix(in srgb, var(--brand) 55%, transparent);
    animation: warmup-sweep var(--dur-pulse) var(--ease-out) infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .seg-fill--warmup {
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

  .seg-row,
  .seg-main {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
  }
  .seg-row {
    min-height: 18px;
  }
  .seg-main {
    flex: 1 1 auto;
  }
  .seg-ico {
    display: grid;
    place-items: center;
    flex: none;
  }
  .seg-ico :global(svg) {
    width: 15px;
    height: 15px;
  }
  .seg-label {
    min-width: 0;
    overflow: hidden;
    font-size: var(--type-sm);
    font-weight: var(--w-heavy);
    letter-spacing: var(--track-tight);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .seg-check {
    margin-left: auto;
    color: var(--ok);
  }
  /* A status reads like the result chips: a dot and a word in its tone. */
  .seg-tag {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-left: auto;
    color: var(--text-soft);
    font: var(--w-strong) var(--type-xs) / 1 var(--font-sans);
  }
  .seg-tag::before {
    content: "";
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
  .seg-tag[data-tone] {
    color: var(--tone);
  }
  @container viz (max-width: 680px) {
    .seg {
      gap: 3px;
      padding: 6px;
    }
    .seg-bar {
      height: 3px;
    }
    /* A tag word takes its own line; a lone check stays beside the label. */
    .seg-row:has(> .seg-tag) {
      display: grid;
      grid-template-rows: 14px 12px;
      gap: 2px;
      align-content: start;
    }
    .seg-main {
      gap: 3px;
    }
    .seg-ico :global(svg) {
      width: 12px;
      height: 12px;
    }
    .seg-label {
      overflow: visible;
      font-size: var(--type-xs);
      line-height: 14px;
    }
    .seg-tag {
      justify-self: start;
      min-width: 0;
      height: 12px;
      margin: 0;
      padding: 0;
      border: 0;
      background: none;
      font: var(--w-normal) var(--type-2xs) / 1 var(--font-sans);
      letter-spacing: 0;
      text-transform: none;
    }
    .seg-check {
      justify-self: start;
      margin: 0;
    }
    .seg-check :global(svg) {
      width: 10px;
      height: 10px;
    }
  }
</style>
