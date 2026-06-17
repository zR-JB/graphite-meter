<script lang="ts">
  /* ============================================================
   * <StageChips> — primary stage selector (§14.2)
   * The accessible default form of stage selection: segmented
   * Latency / Download / Upload chips that live in the main UI,
   * adjacent to the gauge (replaces the old rail <Switch> list).
   *
   * Live-toggle constraints are unchanged (§13.4): toggles route
   * through the store's guarded `toggleStage`, enforcing the
   * ≥1-enabled floor and the future-only rule while a run is in
   * flight. A locked chip shows a short reason ("running" / "done")
   * and is disabled (aria-disabled + native disabled).
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

  function onToggle(stage: StageKey) {
    if (store.toggleStage(stage)) applyStageChange();
  }

  /** Short reason a chip is locked, or null when it is free to toggle. */
  function lockReason(stage: StageKey): string | null {
    if (store.canToggleStage(stage)) return null;
    if (store.phase === stage) return "running";
    const curI = ORDER.indexOf(store.phase as StageKey);
    const stI = ORDER.indexOf(stage);
    return curI >= 0 && stI < curI ? "done" : "locked";
  }
</script>

<fieldset class="stage-chips">
  <legend class="sr-only">Test stages</legend>
  {#each STAGES as s (s.key)}
    {@const on = store.config.stages[s.key]}
    {@const reason = lockReason(s.key)}
    <button
      type="button"
      class="chip"
      class:on
      class:locked={reason !== null}
      role="switch"
      aria-checked={on}
      aria-label="{s.label} stage{reason ? ` (${reason})` : ''}"
      disabled={reason !== null}
      onclick={() => onToggle(s.key)}
    >
      <span class="ico">{@html s.icon}</span>
      <span class="label">{s.label}</span>
      {#if reason}
        <span class="reason">{reason}</span>
      {:else}
        <span class="dot" aria-hidden="true"></span>
      {/if}
    </button>
  {/each}
</fieldset>

<style>
  .stage-chips {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    margin: 0;
    padding: 0;
    border: 0;
  }
  @media (max-width: 479px) {
    .stage-chips {
      grid-template-columns: 1fr;
    }
  }

  .chip {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 44px;
    padding: 0 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    color: var(--text-muted);
    cursor: pointer;
    text-align: left;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out),
      transform var(--dur-hover) var(--ease-out);
  }
  .chip:hover:not(:disabled) {
    border-color: var(--border-strong);
    color: var(--text);
    transform: translateY(-1px);
  }
  .chip:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
  }

  /* Enabled stage — brass-tinted, the value is "on". */
  .chip.on {
    border-color: color-mix(in srgb, var(--brand) 48%, var(--border));
    background: var(--brand-soft);
    color: var(--text);
  }

  .chip.locked {
    cursor: not-allowed;
    opacity: 0.8;
  }

  .ico {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    flex-shrink: 0;
  }
  .ico :global(svg) {
    width: 16px;
    height: 16px;
  }

  .label {
    font-size: 12px;
    font-weight: 700;
    letter-spacing: -0.01em;
    white-space: nowrap;
  }

  /* On-state pip — pushed to the trailing edge. */
  .dot {
    margin-left: auto;
    width: 8px;
    height: 8px;
    border-radius: 999px;
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    transition: background var(--dur-hover) var(--ease-out);
  }
  .chip.on .dot {
    background: var(--brand-strong);
    border-color: var(--brand);
  }

  /* Lock reason — fixed mono tag, mirrors the old rail .stage-reason. */
  .reason {
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
