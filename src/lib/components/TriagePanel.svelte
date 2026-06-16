<script lang="ts">
  /* ============================================================
   * <TriagePanel> — left rail quick config (§3.5)
   * Stage enable toggles, duration presets, run ETA, and the
   * master EngageButton.
   * ============================================================ */
  import Switch from "./Switch.svelte";
  import EngageButton from "./EngageButton.svelte";
  import { console as store, DURATION_PRESETS } from "../state/console.svelte";
  import type { RunnerConfig } from "../runner/contract";

  type PresetKey = "short" | "medium" | "long" | "custom";
  const PRESET_KEYS: PresetKey[] = ["short", "medium", "long", "custom"];

  type Durations = RunnerConfig["duration"];
  function durationsEqual(a: Durations, b: Durations): boolean {
    return (
      a.warmupMs === b.warmupMs &&
      a.latencyMs === b.latencyMs &&
      a.downloadMs === b.downloadMs &&
      a.uploadMs === b.uploadMs
    );
  }

  function detectPreset(): PresetKey {
    for (const k of ["short", "medium", "long"] as const) {
      if (durationsEqual(store.config.duration, DURATION_PRESETS[k])) return k;
    }
    return "custom";
  }

  let preset = $state<PresetKey>(detectPreset());

  function applyPreset(k: PresetKey) {
    preset = k;
    if (k !== "custom") store.config.duration = { ...DURATION_PRESETS[k] };
  }

  // Total run ETA = warmup + each enabled stage's duration.
  const etaMs = $derived(
    store.config.duration.warmupMs +
      (store.config.stages.latency ? store.config.duration.latencyMs : 0) +
      (store.config.stages.download ? store.config.duration.downloadMs : 0) +
      (store.config.stages.upload ? store.config.duration.uploadMs : 0),
  );

  const CUSTOM_FIELDS = [
    { key: "warmupMs", label: "Warmup" },
    { key: "latencyMs", label: "Latency" },
    { key: "downloadMs", label: "Download" },
    { key: "uploadMs", label: "Upload" },
  ] as const;
</script>

<div class="triage flex h-full flex-col gap-5">
  <!-- Stage toggles -->
  <section class="group">
    <h4 class="group-title">Stages</h4>
    <div class="flex flex-col gap-3">
      <Switch bind:checked={store.config.stages.latency} label="Latency" />
      <Switch bind:checked={store.config.stages.download} label="Download" />
      <Switch bind:checked={store.config.stages.upload} label="Upload" />
    </div>
  </section>

  <!-- Duration presets -->
  <section class="group">
    <h4 class="group-title">Duration</h4>
    <div class="tool-group" role="tablist" aria-label="Duration preset">
      {#each PRESET_KEYS as k (k)}
        <button
          role="tab"
          aria-selected={preset === k}
          class="seg"
          class:active={preset === k}
          onclick={() => applyPreset(k)}
        >
          {k[0].toUpperCase() + k.slice(1)}
        </button>
      {/each}
    </div>

    {#if preset === "custom"}
      <div class="custom-grid">
        {#each CUSTOM_FIELDS as f (f.key)}
          <label class="field">
            <span>{f.label}</span>
            <input
              type="number"
              min="0"
              step="500"
              bind:value={store.config.duration[f.key]}
            />
          </label>
        {/each}
      </div>
    {/if}
  </section>

  <div class="flex-1"></div>

  <!-- ETA + engage -->
  <section class="group">
    <div class="eta">
      <span>Est. run</span>
      <span class="eta-val">~{(etaMs / 1000).toFixed(0)}s</span>
    </div>
    <EngageButton />
  </section>
</div>

<style>
  .group-title {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-soft);
    margin: 0 0 10px;
  }

  /* Segmented control — .tool-group pattern */
  .tool-group {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-inset);
  }
  .seg {
    padding: 6px 4px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 600;
    cursor: pointer;
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .seg:hover {
    color: var(--text);
  }
  .seg.active {
    background: var(--surface-2);
    color: var(--text);
    box-shadow: var(--shadow-card);
  }

  .custom-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
    margin-top: 10px;
    animation: slide-down var(--dur-hover) var(--ease-out);
  }
  @keyframes slide-down {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 10px;
    color: var(--text-soft);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .field input {
    width: 100%;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-inset);
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .field input:focus-visible {
    border-color: color-mix(in srgb, var(--brand) 50%, var(--border));
  }

  .eta {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 10px;
    font-size: 11px;
    color: var(--text-soft);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .eta-val {
    font-family: var(--font-mono);
    font-size: 14px;
    font-weight: 700;
    color: var(--text);
    letter-spacing: 0;
  }
</style>
