<script lang="ts">
  // Stage rail keeps editable selection separate from retained run execution.
  import { store, type StageKey } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";
  import { lockReason, stageTrackModel } from "./stageTrack";
  import { failureDetail } from "./failurePresentation";

  const STAGES: {
    key: Exclude<StageKey, "bidirectional">;
    label: string;
    icon: string;
  }[] = [
    { key: "latency", label: "Latency", icon: ICON.ping },
    { key: "download", label: "Download", icon: ICON.download },
    { key: "upload", label: "Upload", icon: ICON.upload },
  ];

  function onToggle(stage: StageKey) {
    controller.toggleStage(stage);
  }

  const segments = $derived.by(() => {
    return STAGES.map((stage) => {
      const execution = store.stagePresentation[stage.key];
      const selected = store.config.stages[stage.key];
      const failure = execution.failure
        ? store.stageFailures[stage.key]
        : undefined;
      const locked = !store.canToggleStage(stage.key);
      const model = stageTrackModel({ selected, locked, execution });
      const reason =
        model.tag ??
        lockReason(
          !locked,
          store.phase,
          store.phaseStage,
          stage.key,
          model.state,
        );
      return { ...stage, ...model, reason, failure };
    });
  });

  // Bidirectional is an advanced Settings choice, not an always-present
  // stage selector. Once disabled it must leave the rail entirely; Settings
  // remains the sole place that can enable it again.
  const bidiPresentation = $derived(store.stagePresentation.bidirectional);
  const bidi = $derived(
    store.config.stages.bidirectional
      ? stageTrackModel({
          selected: true,
          locked: !store.canToggleStage("bidirectional"),
          execution: bidiPresentation,
        })
      : null,
  );
</script>

<fieldset class="stage-track" class:quad={bidi !== null}>
  <legend class="caps"
    >Test stages<span class="sr-only">
      — toggle to include or skip</span
    ></legend
  >
  <!-- The loop variable stays `s`: html-sink-guard.test.ts allowlists the
       `{@html s.icon}` sink by its exact expression text. -->
  {#each segments as s (s.key)}
    <button
      type="button"
      class="seg seg--{s.state}"
      class:on={s.selected}
      role="switch"
      aria-checked={s.selected}
      aria-label="{s.label} stage{s.reason
        ? ` (${s.reason})`
        : s.state === 'complete'
          ? ' (complete)'
          : ''}"
      use:tooltip={s.failure
        ? `${s.label} — ${failureDetail(s.failure.message)}`
        : s.reason
          ? s.reason === "skipped" && !s.locked
            ? `${s.label} — skipped, toggle to include`
            : `${s.label} — ${s.reason}`
          : s.selected
            ? `${s.label} — toggle to skip`
            : `${s.label} — toggle to include`}
      disabled={s.locked}
      onclick={() => onToggle(s.key)}
    >
      <div class="seg-bar" aria-hidden="true">
        {#if s.state === "warmup"}
          <span class="seg-fill seg-fill--warmup"></span>
        {:else if s.state === "failed"}
          <span class="seg-fill seg-fill--failed"></span>
        {:else if s.state === "active" || s.state === "recovering" || s.state === "complete" || s.state === "partial"}
          <span
            class="seg-fill"
            data-tone={s.key}
            class:is-done={s.state === "complete" || s.state === "partial"}
            class:is-stalled={s.state === "recovering"}
            style="--progress:{s.fill / 100}"
          ></span>
        {/if}
      </div>
      <span class="seg-row">
        <span class="seg-main">
          <span class="seg-ico">{@html s.icon}</span>
          <span class="seg-label">{s.label}</span>
        </span>
        {#if s.reason}
          <span class="seg-tag">{s.reason}</span>
        {:else if s.state === "complete"}
          <span class="seg-ico seg-check">{@html ICON.check}</span>
        {/if}
      </span>
    </button>
  {/each}
  {#if bidi}
    <button
      type="button"
      class="seg seg--{bidi.state} on"
      role="switch"
      aria-checked="true"
      aria-label="Bidirectional stage{store.canToggleStage('bidirectional')
        ? ' — toggle to exclude'
        : ' (running)'}"
      use:tooltip={bidiPresentation.failure
        ? `Bi-dir — ${failureDetail(store.stageFailures.bidirectional?.message)}`
        : store.canToggleStage("bidirectional")
          ? "Bidirectional — concurrent down + up. Toggle to exclude (re-enable in Settings)."
          : "Bidirectional — running."}
      disabled={!store.canToggleStage("bidirectional")}
      onclick={() => {
        controller.toggleStage("bidirectional");
      }}
    >
      <div class="seg-bar" aria-hidden="true">
        {#if bidi.state === "warmup"}
          <span class="seg-fill seg-fill--warmup"></span>
        {:else if bidi.state === "failed"}
          <span class="seg-fill seg-fill--failed"></span>
        {:else if bidi.state === "active" || bidi.state === "recovering" || bidi.state === "complete" || bidi.state === "partial"}
          <span
            class="seg-fill"
            data-tone="bidirectional"
            class:is-done={bidi.state === "complete" ||
              bidi.state === "partial"}
            class:is-stalled={bidi.state === "recovering"}
            style="--progress:{bidi.fill / 100}"
          ></span>
        {/if}
      </div>
      <span class="seg-row">
        <span class="seg-main">
          <span class="seg-ico">{@html ICON.bidirectional}</span>
          <span class="seg-label">Bi-dir</span>
        </span>
        {#if bidi.tag}
          <span class="seg-tag">{bidi.tag}</span>
        {:else if bidi.state === "complete"}
          <span class="seg-ico seg-check">{@html ICON.check}</span>
        {/if}
      </span>
    </button>
  {/if}
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
    border: 1px solid var(--border);
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
  .seg--disabled {
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
    transition: transform var(--dur-graph) var(--ease-out);
  }
  .seg-fill.is-done {
    background: var(--ok);
  }
  .seg-fill--failed {
    --progress: 1;
    background: var(--err);
    opacity: 0.45;
  }
  .seg-fill.is-stalled {
    background: var(--err);
    animation: stall-pulse 1100ms var(--ease-out) infinite;
  }
  @keyframes stall-pulse {
    50% {
      opacity: 0.4;
    }
  }
  .seg-fill--warmup {
    --progress: 1;
    width: 45%;
    background: color-mix(in srgb, var(--brand) 55%, transparent);
    animation: warmup-sweep 1100ms var(--ease-out) infinite;
  }
  /* Reduced motion keeps warmup legible as a steady, dimmed bar. */
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
    font-weight: 700;
    letter-spacing: -0.01em;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .seg-check {
    margin-left: auto;
    color: var(--ok);
  }
  .seg-tag {
    display: inline-flex;
    align-items: center;
    height: 18px;
    margin-left: auto;
    padding: 0 6px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    color: var(--text-soft);
    font: 700 var(--type-2xs) / 1 var(--font-mono);
    letter-spacing: var(--track-caps);
    text-transform: uppercase;
  }
  .seg--failed .seg-tag,
  .seg--partial .seg-tag {
    border-color: var(--err-line);
    color: var(--err);
  }
  @container viz (max-width: 680px) {
    .seg {
      gap: 3px;
      padding: 6px;
    }
    .seg-bar {
      height: 3px;
    }
    .seg-row {
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
