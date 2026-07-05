<script lang="ts">
  /* ============================================================
   * <StageTrack> — selection + progress in one element
   * The single combined stage control: Latency / Download / Upload
   * each render as ONE segment that is simultaneously
   *   (a) a selectable switch — toggle enable/disable, routed through
   *       the store's guarded `toggleStage`/`canToggleStage` +
   *       `applyStageChange`, keeping the ≥1-enabled floor and the
   *       future-only rule while a run is in flight; and
   *   (b) a live progress indicator — the active stage fills by
   *       `phaseFraction`, completed stages settle into a done state,
   *       pending enabled stages stay neutral.
   *
   * Warmup is NOT a standalone segment: each stage now owns a warmup
   * lead-in, so during `phase === "warmup"` the *upcoming* stage (the
   * first enabled stage not yet measured) shows a subtle indeterminate
   * "warming up" shimmer (reduced-motion safe) while already-measured
   * stages settle to done. Completion is NOT a separate ✓ block: on
   * `complete` the track simply settles with each run stage in its done
   * state. Deselected stages still render (muted) so they can be
   * re-enabled.
   * ============================================================ */
  import { store, type StageKey } from "../state/store.svelte";
  import { applyStageChange } from "../runner/wire.svelte";
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";

  const STAGES: { key: StageKey; label: string; icon: string }[] = [
    { key: "latency", label: "Latency", icon: ICON.ping },
    { key: "download", label: "Download", icon: ICON.download },
    { key: "upload", label: "Upload", icon: ICON.upload },
  ];

  const ORDER: StageKey[] = ["latency", "download", "upload"];

  type SegState =
    | "disabled" // deselected — muted, re-enableable
    | "warmup" // first enabled stage during lead-in (indeterminate)
    | "active" // currently running — fills by phaseFraction
    | "done" // finished this run — settled fill
    | "failed" // skipped — couldn't run (see store.stageFailures)
    | "pending"; // enabled, not yet reached

  function onToggle(stage: StageKey) {
    if (store.toggleStage(stage)) applyStageChange();
  }

  /** Short reason a segment can't toggle, or null when it is free. */
  function lockReason(stage: StageKey): string | null {
    if (store.canToggleStage(stage)) return null;
    if (store.phase === stage) return "running";
    const curI = ORDER.indexOf(store.phase as StageKey);
    const stI = ORDER.indexOf(stage);
    return curI >= 0 && stI < curI ? "done" : "upcoming";
  }

  // First enabled stage in run order.
  const firstEnabled = $derived(
    ORDER.find((k) => store.config.stages[k]) ?? null,
  );

  // A stage is "done" once its per-stage result has landed — that event fires
  // the instant a measured phase ends, so it's the direct "finished" signal.
  const isDone = (k: StageKey) => store.stageResults[k] != null;
  // The stage a warmup is leading into: first enabled stage not yet measured.
  const upcoming = $derived(
    ORDER.find((k) => store.config.stages[k] && !isDone(k)) ?? firstEnabled,
  );

  const segs = $derived.by(() => {
    const cur = store.phase;
    const curI = ORDER.indexOf(cur as StageKey); // -1 unless running a stage
    return STAGES.map((s) => {
      const enabled = store.config.stages[s.key];
      const failure = store.stageFailures[s.key];
      // Enabled stages keep the running/done/upcoming logic; deselected stages
      // read "skipped" once a run has started, no tag while idle.
      const reason = enabled
        ? failure
          ? "failed"
          : lockReason(s.key)
        : store.phase !== "idle"
          ? "skipped"
          : null;
      // Interaction lock is decoupled from the tag: a "skipped" future stage
      // stays clickable so it can be re-included.
      const locked = !store.canToggleStage(s.key);
      let state: SegState;
      let fill = 0;
      if (!enabled) {
        state = "disabled";
      } else if (failure) {
        state = "failed";
      } else if (cur === "complete") {
        state = "done";
        fill = 100;
      } else if (cur === "warmup") {
        // Warmup precedes a specific stage now: measured stages settle to done,
        // the upcoming stage shimmers, the rest stay pending.
        if (isDone(s.key)) {
          state = "done";
          fill = 100;
        } else {
          state = s.key === upcoming ? "warmup" : "pending";
        }
      } else if (curI === -1) {
        // idle / aborted / error — neutral, selectable.
        state = "pending";
      } else {
        const stI = ORDER.indexOf(s.key);
        if (stI < curI) {
          state = "done";
          fill = 100;
        } else if (stI === curI) {
          // Active phase: while stalled the fill freezes (phaseFraction is
          // frozen in the core) and pulses to signal the link is down.
          // Quantized to 0.5% so the 50 Hz progress stream doesn't restyle
          // the bar every tick.
          state = "active";
          fill = Math.round(store.phaseFraction * 200) / 2;
        } else {
          state = "pending";
        }
      }
      return { ...s, enabled, reason, locked, state, fill, failure };
    });
  });

  // Bidirectional is the advanced 4th stage — can only be ENABLED in Settings
  // (it renders here only once Settings has turned it on), but CAN be disabled
  // from this track: clicking it flips the config off, which makes this block
  // return null next render and the segment disappears — re-enabling is
  // Settings-only again (see canDisableBidirectional/disableBidirectional in
  // store.svelte.ts). It always runs last, so its state is simply
  // pending → active (during its phase) → done.
  const bidi = $derived.by<{ state: SegState; fill: number } | null>(() => {
    if (!store.config.stages.bidirectional) return null;
    if (store.stageFailures.bidirectional) return { state: "failed", fill: 0 };
    const p = store.phase;
    if (p === "complete") return { state: "done", fill: 100 };
    if (p === "bidirectional")
      return {
        state: "active",
        fill: Math.round(store.phaseFraction * 200) / 2,
      };
    return { state: "pending", fill: 0 };
  });

  // 3 stages share one row; with the optional 4th (bidirectional) segment the
  // track wraps to 2×2 only below the compact width (see .quad's @container).
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
        {#if bidi.state === "failed"}
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
  /* 3 segments share one row; 4 (.quad) wrap to 2×2 only below the compact
     width. A narrow docked-panel stage degrades via .seg-label's ellipsis. */
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
  /* Queries the nearest ancestor container (GaugePanel's viz container).
     430px mirrors --bp-compact (app.css). */
  @container (max-width: 430px) {
    .stage-track.quad {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  /* Each segment is the chip AND the progress lane. */
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

  /* Enabled (selected) — brass-tinted, the value is "on". */
  .seg.on {
    border-color: color-mix(in srgb, var(--brand) 48%, var(--border));
    background: var(--brand-soft);
    color: var(--text);
  }

  /* Disabled / deselected — de-emphasized but still present + re-enableable. */
  .seg--disabled {
    opacity: 0.5;
  }
  .seg:disabled {
    cursor: not-allowed;
  }

  /* The progress lane that lives at the top of every segment. */
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
  /* Settled done state — a calm, uniform fill regardless of phase tint. */
  .seg-fill.is-done {
    background: var(--ok);
  }
  /* Skipped stage — a muted full err band, steady (nothing is in flight). */
  .seg-fill--failed {
    width: 100%;
    background: var(--err);
    opacity: 0.45;
  }
  /* Stalled (link dropped mid-phase): the fill is frozen — pulse it in an error
     tint so a stuck progress bar reads as "paused, reconnecting", not hung. */
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

  /* Warmup lead-in — indeterminate sweep on the first enabled stage,
     folded in here instead of a standalone "W" segment. */
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
    /* Reduced-motion: a steady, soft indeterminate band, no travel. */
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

  /* Single line always: the label ellipsizes (.seg-label) rather than the
     tag/check wrapping to a second row, so every segment keeps one height. */
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
  /* Settled done glyph — the auto margin right-aligns it on whichever flex
     line it lands on. */
  .seg-check {
    margin-left: auto;
    color: var(--ok);
  }

  /* Lock reason tag. */
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
