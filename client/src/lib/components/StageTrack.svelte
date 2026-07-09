<script lang="ts">
  import { MEASURED_STAGES, store, type StageKey } from "../state/store.svelte";
  import { applyStageChange } from "../runner/wire.svelte";
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";

  const STAGES: { key: StageKey; label: string; icon: string }[] = [
    { key: "latency", label: "Latency", icon: ICON.ping },
    { key: "download", label: "Download", icon: ICON.download },
    { key: "upload", label: "Upload", icon: ICON.upload },
  ];

  const TRACK_ORDER = [...MEASURED_STAGES, "bidirectional"] as const;
  type TrackStageKey = (typeof TRACK_ORDER)[number];

  type SegState =
    "disabled" | "warmup" | "active" | "done" | "failed" | "pending";

  const stageIndex = (stage: TrackStageKey | null) =>
    stage ? TRACK_ORDER.indexOf(stage) : -1;

  const progressFill = () => Math.round(store.phaseFraction * 200) / 2;

  function segmentState(
    stage: StageKey,
    enabled: boolean,
    failed: boolean,
    curI: number,
  ): { state: SegState; fill: number } {
    if (!enabled) return { state: "disabled", fill: 0 };
    if (failed) return { state: "failed", fill: 0 };
    if (store.phase === "complete") return { state: "done", fill: 100 };
    const stI = TRACK_ORDER.indexOf(stage);
    if (store.phase === "warmup") {
      if (stI < curI) return { state: "done", fill: 100 };
      if (stI === curI) return { state: "warmup", fill: 0 };
      return { state: "pending", fill: 0 };
    }
    if (curI === -1) return { state: "pending", fill: 0 };
    if (stI < curI) return { state: "done", fill: 100 };
    if (stI === curI) return { state: "active", fill: progressFill() };
    return { state: "pending", fill: 0 };
  }

  function onToggle(stage: StageKey) {
    if (store.toggleStage(stage)) applyStageChange();
  }

  function lockReason(stage: StageKey, state: SegState): string | null {
    if (store.canToggleStage(stage)) return null;
    if (state === "done") return "done";
    if (store.phase === stage) return "running";
    const curI = stageIndex(store.phaseStage);
    const stI = TRACK_ORDER.indexOf(stage);
    return curI >= 0 && stI < curI ? "done" : "upcoming";
  }

  const segs = $derived.by(() => {
    const curI = stageIndex(store.phaseStage);
    return STAGES.map((s) => {
      const enabled = store.config.stages[s.key];
      const failure = store.stageFailures[s.key];
      const locked = !store.canToggleStage(s.key);
      const { state, fill } = segmentState(s.key, enabled, !!failure, curI);
      const reason = enabled
        ? failure
          ? "failed"
          : lockReason(s.key, state)
        : store.phase !== "idle"
          ? "skipped"
          : null;
      return { ...s, enabled, reason, locked, state, fill, failure };
    });
  });

  const bidi = $derived.by<{ state: SegState; fill: number } | null>(() => {
    if (!store.config.stages.bidirectional) return null;
    if (store.stageFailures.bidirectional) return { state: "failed", fill: 0 };
    const p = store.phase;
    if (p === "complete") return { state: "done", fill: 100 };
    if (p === "warmup" && store.phaseStage === "bidirectional")
      return { state: "warmup", fill: 0 };
    if (p === "bidirectional")
      return {
        state: "active",
        fill: progressFill(),
      };
    return { state: "pending", fill: 0 };
  });

  const totalSegs = $derived(segs.length + (bidi ? 1 : 0));
  const quad = $derived(totalSegs >= 4);
</script>

<fieldset class="stage-track" class:quad>
  <legend class="sr-only">Test stages — tap to enable or disable</legend>
  {#each segs as s (s.key)}
    <button
      type="button"
      class="seg seg--{s.state}"
      class:on={s.enabled}
      role="switch"
      aria-checked={s.enabled}
      aria-label="{s.label} stage{s.reason ? ` (${s.reason})` : ''}"
      use:tooltip={s.failure
        ? `${s.label} — ${s.failure.message}`
        : s.reason
          ? s.reason === "skipped" && !s.locked
            ? `${s.label} — skipped, tap to include`
            : `${s.label} — ${s.reason}`
          : s.enabled
            ? `${s.label} — tap to skip`
            : `${s.label} — tap to include`}
      disabled={s.locked}
      onclick={() => onToggle(s.key)}
    >
      <div class="seg-bar" aria-hidden="true">
        {#if s.state === "warmup"}
          <span class="seg-fill seg-fill--warmup"></span>
        {:else if s.state === "failed"}
          <span class="seg-fill seg-fill--failed"></span>
        {:else if s.state === "active" || s.state === "done"}
          <span
            class="seg-fill seg-fill--{s.key}"
            class:is-done={s.state === "done"}
            class:is-stalled={s.state === "active" && !store.measuring}
            style="width:{s.fill}%"
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
        {:else if s.state === "done"}
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
      aria-label="Bidirectional stage{store.canDisableBidirectional()
        ? ' — tap to exclude'
        : ' (running)'}"
      use:tooltip={store.stageFailures.bidirectional
        ? `Bi-dir — ${store.stageFailures.bidirectional.message}`
        : store.canDisableBidirectional()
          ? "Bidirectional — concurrent down + up. Tap to exclude (re-enable in Settings)."
          : "Bidirectional — running."}
      disabled={!store.canDisableBidirectional()}
      onclick={() => {
        if (store.disableBidirectional()) applyStageChange();
      }}
    >
      <div class="seg-bar" aria-hidden="true">
        {#if bidi.state === "warmup"}
          <span class="seg-fill seg-fill--warmup"></span>
        {:else if bidi.state === "failed"}
          <span class="seg-fill seg-fill--failed"></span>
        {:else if bidi.state === "active" || bidi.state === "done"}
          <span
            class="seg-fill seg-fill--bidirectional"
            class:is-done={bidi.state === "done"}
            class:is-stalled={bidi.state === "active" && !store.measuring}
            style="width:{bidi.fill}%"
          ></span>
        {/if}
      </div>
      <span class="seg-row">
        <span class="seg-main">
          <span class="seg-ico">{@html ICON.bidirectional}</span>
          <span class="seg-label">Bi-dir</span>
        </span>
        {#if bidi.state === "done"}
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
    margin: 0;
    padding: 0;
    border: 0;
  }
  .stage-track.quad {
    grid-template-columns: repeat(4, minmax(0, 1fr));
  }
  @container (max-width: 430px) {
    .stage-track.quad {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  .seg {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    min-height: 40px;
    padding: var(--space-2) var(--space-2) var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: var(--elev-tile);
    color: var(--text-muted);
    cursor: pointer;
    text-align: left;
    overflow: hidden;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out),
      opacity var(--dur-hover) var(--ease-out),
      transform var(--dur-hover) var(--ease-out);
  }
  .seg:hover:not(:disabled) {
    border-color: var(--border-strong);
    color: var(--text);
    transform: translateY(-1px);
  }
  .seg:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
  }

  .seg.on {
    border-color: color-mix(in srgb, var(--brand) 48%, var(--border));
    background: var(--brand-soft);
    color: var(--text);
  }

  .seg--disabled {
    opacity: 0.5;
  }
  .seg:disabled {
    cursor: not-allowed;
  }

  .seg-bar {
    position: relative;
    width: 100%;
    height: 5px;
    border-radius: var(--radius-xs);
    background: var(--surface-inset);
    overflow: hidden;
  }
  .seg-fill {
    position: absolute;
    inset: 0 auto 0 0;
    height: 100%;
    width: 0;
    border-radius: inherit;
    transition: width var(--dur-graph) var(--ease-out);
  }
  .seg-fill--latency {
    background: var(--phase-latency);
  }
  .seg-fill--download {
    background: var(--phase-download);
  }
  .seg-fill--upload {
    background: var(--phase-upload);
  }
  .seg-fill--bidirectional {
    background: var(--phase-bidirectional);
  }
  .seg-fill.is-done {
    background: var(--ok);
  }
  .seg-fill--failed {
    width: 100%;
    background: var(--err);
    opacity: 0.45;
  }
  .seg-fill.is-stalled {
    background: var(--err);
  }
  @media (prefers-reduced-motion: no-preference) {
    .seg-fill.is-stalled {
      animation: stall-pulse 1100ms var(--ease-out) infinite;
    }
  }
  @keyframes stall-pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.4;
    }
  }

  .seg-fill--warmup {
    width: 45%;
    background: color-mix(in srgb, var(--brand) 55%, transparent);
  }
  @media (prefers-reduced-motion: no-preference) {
    .seg-fill--warmup {
      animation: warmup-sweep 1100ms var(--ease-out) infinite;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .seg-fill--warmup {
      width: 100%;
      opacity: 0.55;
    }
  }
  @keyframes warmup-sweep {
    0% {
      transform: translateX(-110%);
    }
    100% {
      transform: translateX(240%);
    }
  }

  .seg-row {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }
  .seg-main {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    flex: 1 1 auto;
  }
  .seg-ico {
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
  }
  .seg-ico :global(svg) {
    width: 15px;
    height: 15px;
  }
  .seg-label {
    font-size: 12px;
    font-weight: 700;
    letter-spacing: -0.01em;
    white-space: nowrap;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .seg-check {
    margin-left: auto;
    color: var(--ok);
  }

  .seg-tag {
    margin-left: auto;
    padding: 2px 6px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-xs);
    background: var(--surface-inset);
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .seg--failed .seg-tag {
    border-color: color-mix(in srgb, var(--err) 35%, var(--border-subtle));
    color: var(--err);
  }
</style>
