<script lang="ts">
  /* ============================================================
   * <StageTrack> — selection + progress in one element (§14.2)
   * The single combined stage control: Latency / Download / Upload
   * each render as ONE segment that is simultaneously
   *   (a) a selectable switch — toggle enable/disable, routed through
   *       the store's guarded `toggleStage`/`canToggleStage` +
   *       `applyStageChange`, keeping the ≥1-enabled floor and the
   *       future-only rule while a run is in flight (§13.4); and
   *   (b) a live progress indicator — the active stage fills by
   *       `phaseFraction`, completed stages settle into a done state,
   *       pending enabled stages stay neutral.
   *
   * Warmup is NOT a standalone segment: during `phase === "warmup"`
   * the first enabled stage shows a subtle indeterminate "warming up"
   * shimmer (reduced-motion safe). Completion is NOT a separate ✓
   * block: on `complete` the track simply settles with each run stage
   * in its done state. Deselected stages still render (muted) so they
   * can be re-enabled.
   * ============================================================ */
  import {
    console as store,
    type StageKey,
  } from "../state/console.svelte";
  import { applyStageChange } from "../runner/wire.svelte";
  import { ICON } from "../constants";

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

  // First enabled stage in run order — receives the warmup lead-in.
  const firstEnabled = $derived(
    ORDER.find((k) => store.config.stages[k]) ?? null,
  );

  const segs = $derived.by(() => {
    const cur = store.phase;
    const curI = ORDER.indexOf(cur as StageKey); // -1 unless running a stage
    return STAGES.map((s) => {
      const enabled = store.config.stages[s.key];
      // Tag: enabled stages keep the running/done/upcoming logic; a deselected
      // stage reads "skipped" once a run has started, no tag while idle (§14.x).
      const reason = enabled
        ? lockReason(s.key)
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
      } else if (cur === "complete") {
        state = "done";
        fill = 100;
      } else if (cur === "warmup") {
        state = s.key === firstEnabled ? "warmup" : "pending";
      } else if (curI === -1) {
        // idle / aborted / error — neutral, selectable.
        state = "pending";
      } else {
        const stI = ORDER.indexOf(s.key);
        if (stI < curI) {
          state = "done";
          fill = 100;
        } else if (stI === curI) {
          state = "active";
          fill = store.phaseFraction * 100;
        } else {
          state = "pending";
        }
      }
      return { ...s, enabled, reason, locked, state, fill };
    });
  });
</script>

<fieldset class="stage-track">
  <legend class="sr-only">Test stages — tap to enable or disable</legend>
  {#each segs as s (s.key)}
    <button
      type="button"
      class="seg seg--{s.state}"
      class:on={s.enabled}
      role="switch"
      aria-checked={s.enabled}
      aria-label="{s.label} stage{s.reason ? ` (${s.reason})` : ''}"
      title={s.reason
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
        {:else if s.state === "active" || s.state === "done"}
          <span
            class="seg-fill seg-fill--{s.key}"
            class:is-done={s.state === "done"}
            style="width:{s.fill}%"
          ></span>
        {/if}
      </div>
      <span class="seg-row">
        <span class="seg-ico">{@html s.icon}</span>
        <span class="seg-label">{s.label}</span>
        {#if s.reason}
          <span class="seg-tag">{s.reason}</span>
        {:else if s.state === "done"}
          <span class="seg-ico seg-check">{@html ICON.check}</span>
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
    margin: 0;
    padding: 0;
    border: 0;
  }
  @media (max-width: 479px) {
    .stage-track {
      grid-template-columns: 1fr;
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
  /* Settled done state — a calm, uniform fill regardless of phase tint. */
  .seg-fill.is-done {
    background: var(--ok);
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

  .seg-row {
    display: flex;
    align-items: center;
    gap: 7px;
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
  }
  /* Settled done glyph — subtle, replaces the retired standalone ✓ segment. */
  .seg-check {
    margin-left: auto;
    color: var(--ok);
  }

  /* Lock reason tag — mirrors the old rail .stage-reason. */
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
</style>
