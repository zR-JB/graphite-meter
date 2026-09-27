<script lang="ts">
  import { catalogSelection } from "../../presentation/serverAppearance";
  import { store } from "../../state/store.svelte";
  import {
    clampDuration,
    DURATION_LIMITS,
    DURATION_PRESETS,
  } from "../../state/defaults";
  import type { PingCadence, RunnerConfig } from "../../runner/contract";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  import { pathOptions } from "../../presentation/paths";
  import { normalizeStreamCount } from "../../runner/paths";
  import { tooltip } from "../../actions/tooltip";
  import Icon from "../Icon.svelte";
  import Switch from "../Switch.svelte";
  import ServerSelection from "../ServerSelection.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";
  import {
    BLOCKED,
    JARGON,
    phaseLabel,
    PING_CADENCE,
    READINESS,
    READINESS_TIP,
    STAGE,
  } from "../../presentation/vocabulary";
  import { fmtDuration } from "../../format";
  import { untrack } from "svelte";
  import {
    announce,
    announceChanges,
  } from "../../presentation/announcer.svelte";
  import ConfirmDialog from "../ConfirmDialog.svelte";

  let {
    open,
    onOpenHistory,
  }: { open: boolean; onOpenHistory: (invoker: HTMLElement) => void } =
    $props();
  const running = $derived(store.isRunning);
  let resetConfirmOpen = $state(false);
  // The panel stays mounted when closed; server discovery runs only while it shows.
  $effect(() => {
    if (
      open &&
      store.serverCatalog &&
      !store.catalogLoading &&
      !store.isRunning &&
      !store.preparing
    ) {
      untrack(() => controller.loadServerMetadata());
      return () => controller.cancelServerMetadata();
    }
  });
  // A confirmation belongs to the visible panel; closing the panel dismisses it.
  $effect(() => {
    if (!open) resetConfirmOpen = false;
  });
  function resetSettings() {
    resetConfirmOpen = false;
    controller.restoreDefaults();
    customDuration = false;
  }

  // The caution follows the selected datagram card, not the toggle.
  const datagramSelected = $derived(
    store.connections.throughput.target?.transport === "webtransport-datagram",
  );
  const simultaneous = $derived(store.selectedServers.length > 1);
  const selectedServers = $derived(
    catalogSelection(store.serverCatalog, store.selectedServers),
  );
  const throughputPath = $derived(store.connections.throughput);
  const throughputTargets = $derived(
    pathOptions(
      "throughput",
      selectedServers,
      store.servers,
      store.config,
      throughputPath.target
        ? {
            id: throughputPath.target.id,
            protocol: throughputPath.observedProtocol,
          }
        : undefined,
      simultaneous,
    ),
  );
  const latencyTargets = $derived(
    pathOptions(
      "latency",
      store.latencySelection.mode === "primary"
        ? selectedServers.filter(
            (server) => server.id === store.primaryLatencyServer,
          )
        : selectedServers,
      store.servers,
      store.config,
      undefined,
      simultaneous,
    ),
  );

  const CADENCES = [
    ["pingCadence", "Idle latency cadence", JARGON.idleCadence],
    ["loadedPingCadence", "Loaded latency cadence", JARGON.loadedCadence],
  ] as const;
  type Preset = "short" | "medium" | "long" | "custom";
  const PRESETS: Preset[] = ["short", "medium", "long", "custom"];
  const DURATION_FIELDS = [
    ["warmupMs", phaseLabel("warmup")],
    ["latencyMs", STAGE.latency.label],
    ["downloadMs", STAGE.download.label],
    ["uploadMs", STAGE.upload.label],
    ["bidirectionalMs", STAGE.bidirectional.label],
  ] as const;
  type DurationKey = (typeof DURATION_FIELDS)[number][0];
  function sameDuration(
    a: RunnerConfig["duration"],
    b: RunnerConfig["duration"],
  ) {
    return DURATION_FIELDS.every(([key]) => a[key] === b[key]);
  }
  let customDuration = $state(false);
  const durationMode = $derived.by((): Preset => {
    if (customDuration) return "custom";
    for (const key of ["short", "medium", "long"] as const)
      if (sameDuration(store.config.duration, DURATION_PRESETS[key]))
        return key;
    return "custom";
  });
  function setPreset(preset: Preset) {
    customDuration = preset === "custom";
    if (preset !== "custom") {
      controller.configureRun({ duration: { ...DURATION_PRESETS[preset] } });
    }
  }
  let rejected = $state<"duration" | "gauge" | "streams" | null>(null);
  function commitNumber(
    event: Event,
    field: NonNullable<typeof rejected>,
    current: number,
    normalize: (value: number) => number,
    commit: (value: number) => boolean,
  ) {
    const input = event.currentTarget as HTMLInputElement;
    const raw = input.valueAsNumber;
    const value = Number.isFinite(raw) ? normalize(raw) : current;
    const accepted = value === current || commit(value);
    input.value = String(accepted ? value : current);
    rejected = accepted ? null : field;
    if (!accepted) announce(rejection);
  }
  const rejection = $derived(
    store.startError || "This change cannot apply to the current run.",
  );
  function setDuration(key: DurationKey, event: Event) {
    commitNumber(
      event,
      "duration",
      store.config.duration[key],
      (value) => clampDuration(key, value),
      (value) =>
        controller.configureRun({
          duration: { ...store.config.duration, [key]: value },
        }),
    );
  }
  function setBidirectional(enabled: boolean) {
    controller.configureRun({
      stages: { ...store.config.stages, bidirectional: enabled },
    });
  }
  const activeDurationFields = $derived(
    store.config.stages.bidirectional
      ? DURATION_FIELDS
      : DURATION_FIELDS.filter(([key]) => key !== "bidirectionalMs"),
  );
  function presetTip(preset: Preset) {
    const name = preset[0].toUpperCase() + preset.slice(1);
    if (preset === "custom") return `${name}\nSet each stage's time`;
    const times = DURATION_PRESETS[preset];
    return `${name}\n${activeDurationFields
      .map(([key, label]) => `${label} ${fmtDuration(times[key])}`)
      .join(" · ")}`;
  }

  const forced = $derived(store.config.transferStreams.mode === "forced");
  const streams = (patch: Partial<RunnerConfig["transferStreams"]>) =>
    controller.configureRun({
      transferStreams: { ...store.config.transferStreams, ...patch },
    });
  const gaugeMax = (throughputMaxBytesPerSec: number | "auto") =>
    controller.configureRun({ visualization: { throughputMaxBytesPerSec } });

  const vizAuto = $derived(
    store.config.visualization.throughputMaxBytesPerSec === "auto",
  );
  const vizDisplay = $derived(
    vizAuto
      ? 0
      : store.toUnit(
          store.config.visualization.throughputMaxBytesPerSec as number,
        ),
  );
  function setVizAuto(auto: boolean) {
    gaugeMax(
      auto ? "auto" : Math.max(1, Math.round(store.scales.chartBytesPerSec)),
    );
  }
  function setVizMax(event: Event) {
    const current = Number(vizDisplay.toFixed(2));
    commitNumber(
      event,
      "gauge",
      current,
      (value) => (value > 0 ? value : current),
      (value) => gaugeMax(Math.max(1, Math.round(store.fromUnit(value)))),
    );
  }

  const readiness = $derived(
    store.startBlocker ? "blocked" : store.selectionValidation,
  );
  // Settings stays mounted: only a path problem is worth interrupting for.
  announceChanges(() =>
    READINESS[readiness].tone === "err" || readiness === "blocked"
      ? `Connection paths: ${READINESS[readiness].label}`
      : "",
  );
</script>

{#snippet rejectedHint(field: typeof rejected)}
  {#if rejected === field}<p class="notice" data-tone="warn">
      {rejection}
    </p>{/if}
{/snippet}

{#snippet toggle(
  label: string,
  tip: string,
  checked: boolean,
  onToggle: (next: boolean) => void,
  disabled = false,
)}
  <div class="switch-row">
    <Switch {checked} {onToggle} {disabled} {label} tooltip={tip} />
  </div>
{/snippet}

<div class="settings">
  <section class="group">
    <div class="group-head">
      <h3 class="caps">Connection</h3>
      <span
        class="badge"
        data-readiness={readiness}
        data-tone={READINESS[readiness].tone}
        {@attach tooltip(() =>
          readiness === "blocked"
            ? `${BLOCKED}\n${store.startBlocker}`
            : READINESS_TIP[readiness],
        )}
      >
        {READINESS[readiness].label}
      </span>
    </div>
    <ServerSelection />
    <ConnectionPicker
      role="throughput"
      options={throughputTargets}
      locked={running || store.preparing}
    />
    <ConnectionPicker
      role="latency"
      options={latencyTargets}
      locked={running || store.preparing}
    />
  </section>

  <section class="group">
    <h3 class="caps">
      <span {@attach tooltip(() => JARGON.stageTime)}>Duration</span>
    </h3>
    <div class="kv">
      <div>
        <div
          class="segmented presets"
          role="group"
          aria-label="Duration preset"
        >
          {#each PRESETS as preset}
            <button
              type="button"
              aria-pressed={durationMode === preset}
              disabled={store.preparing}
              {@attach tooltip(() => presetTip(preset))}
              onclick={() => setPreset(preset)}>{preset}</button
            >
          {/each}
        </div>
      </div>
      {#each activeDurationFields as [key, label] (key)}
        {@const tip = key === "warmupMs" ? JARGON.warmup : ""}
        {#if durationMode === "custom"}
          <div>
            <label class="row">
              <span {@attach tip ? tooltip(() => tip) : null}>{label}</span>
              <span class="number">
                <input
                  type="number"
                  min="0"
                  max={DURATION_LIMITS[key][1]}
                  step="500"
                  disabled={store.preparing}
                  value={store.config.duration[key]}
                  onchange={(event) => setDuration(key, event)}
                />
                <span>ms</span>
              </span>
            </label>
          </div>
        {:else}
          <div class="row">
            <span {@attach tip ? tooltip(() => tip) : null}>{label}</span>
            <span class="value"
              >{fmtDuration(DURATION_PRESETS[durationMode][key])}</span
            >
          </div>
        {/if}
      {/each}
      {@render toggle(
        "Bidirectional stage",
        JARGON.bidirectionalStage,
        store.config.stages.bidirectional,
        setBidirectional,
        store.preparing || (running && store.phaseStage === "bidirectional"),
      )}
      {@render toggle(
        "Finish stable stages early",
        JARGON.earlyFinish,
        store.config.adaptive,
        (adaptive) => controller.configureRun({ adaptive }),
        store.preparing,
      )}
    </div>
    {@render rejectedHint("duration")}
    {#if running}
      <p class="hint">Changes apply to the current and unstarted stages.</p>
    {/if}
  </section>

  <section class="group">
    <h3 class="caps">Display</h3>
    <div class="kv">
      <div class="row">
        <span>Rate unit</span>
        <div class="segmented" role="group" aria-label="Rate unit">
          <button
            type="button"
            aria-pressed={store.unitKind === "bits"}
            {@attach tooltip(() => JARGON.unitBits)}
            onclick={() => store.prefer({ unitKind: "bits" })}>Bits</button
          >
          <button
            type="button"
            aria-pressed={store.unitKind === "bytes"}
            {@attach tooltip(() => JARGON.unitBytes)}
            onclick={() => store.prefer({ unitKind: "bytes" })}>Bytes</button
          >
        </div>
      </div>
      <div class="row">
        <span>Prefix</span>
        <div class="segmented" role="group" aria-label="Prefix scale">
          <button
            type="button"
            aria-pressed={store.unitBase === "base10"}
            {@attach tooltip(() => JARGON.unitDecimal)}
            onclick={() => store.prefer({ unitBase: "base10" })}>Decimal</button
          >
          <button
            type="button"
            aria-pressed={store.unitBase === "base2"}
            {@attach tooltip(() => JARGON.unitBinary)}
            onclick={() => store.prefer({ unitBase: "base2" })}>Binary</button
          >
        </div>
      </div>
      {@render toggle(
        "Show estimated wire rate",
        JARGON.wireRate,
        store.showWireEstimates,
        (showWireEstimates) => store.prefer({ showWireEstimates }),
      )}
      {@render toggle(
        "Keyboard shortcuts",
        JARGON.keyShortcuts,
        store.keyShortcuts,
        (keyShortcuts) => store.prefer({ keyShortcuts }),
      )}
      {@render toggle(
        "Scale throughput automatically",
        JARGON.gaugeAuto,
        vizAuto,
        setVizAuto,
      )}
      {#if !vizAuto}
        <div>
          <label class="row">
            <span {@attach tooltip(() => JARGON.gaugeMax)}>Maximum</span>
            <span class="number">
              <input
                type="number"
                min="1"
                value={Number(vizDisplay.toFixed(2))}
                onchange={setVizMax}
              />
              <span>{store.unitLabel}</span>
            </span>
          </label>
        </div>
      {/if}
    </div>
    {@render rejectedHint("gauge")}
  </section>

  <section class="group">
    <div class="group-head">
      <h3 class="caps">History</h3>
      <a
        class="btn btn-quiet"
        href="#/history"
        onclick={(event) => {
          event.preventDefault();
          onOpenHistory(event.currentTarget as HTMLElement);
        }}><Icon name="history" />Open History</a
      >
    </div>
    <div class="kv">
      {@render toggle(
        "Save completed results on this device",
        JARGON.saveResults,
        store.savingResults,
        (enabled) =>
          store.prefer({
            resultHistoryPreference: enabled ? "enabled" : "disabled",
          }),
      )}
    </div>
  </section>

  <section class="group">
    <h3 class="caps">Latency probes</h3>
    <div class="kv">
      {#each CADENCES as [key, label, tip] (key)}
        <div>
          <label class="row">
            <span {@attach tooltip(() => tip)}>{label}</span>
            <select
              value={store.config[key]}
              onchange={(event) =>
                controller.configureRun({
                  [key]: event.currentTarget.value as PingCadence,
                })}
              disabled={running || store.preparing}
            >
              {#each Object.entries(PING_CADENCE) as [value, name] (value)}
                <option {value}>{name}</option>
              {/each}
            </select>
          </label>
        </div>
      {/each}
      {@render toggle(
        "Skip loaded latency when latency is off",
        JARGON.skipLoadedLatency,
        store.config.skipLoadedLatencyWhenStageOff,
        (skipLoadedLatencyWhenStageOff) =>
          controller.configureRun({ skipLoadedLatencyWhenStageOff }),
        running || store.preparing,
      )}
    </div>
  </section>

  <section class="group">
    <h3 class="caps">Transfers</h3>
    <div class="kv">
      {@render toggle(
        "Force exact stream count",
        JARGON.forcedStreams,
        forced,
        (on) => streams({ mode: on ? "forced" : "auto" }),
        running || store.preparing,
      )}
      <div>
        <label class="row">
          <span
            {@attach tooltip(() =>
              forced ? JARGON.forcedStreamCount : JARGON.autoStreamCount,
            )}
            >{forced
              ? "Streams per server and direction"
              : "Maximum H1 streams per direction"}</span
          >
          <input
            type="number"
            min="1"
            max="128"
            step="1"
            disabled={running || store.preparing}
            value={store.config.transferStreams.count}
            onchange={(event) =>
              commitNumber(
                event,
                "streams",
                store.config.transferStreams.count,
                normalizeStreamCount,
                (count) => streams({ count }),
              )}
          />
        </label>
      </div>
      {@render toggle(
        "Datagram throughput (experimental)",
        JARGON.datagramThroughput,
        store.config.experimentalDatagramThroughput,
        (experimentalDatagramThroughput) =>
          controller.configureRun({ experimentalDatagramThroughput }),
        running || store.preparing,
      )}
    </div>
    {@render rejectedHint("streams")}
    {#if store.streamPlanError}
      <p class="notice" data-tone="warn" role="status">
        {store.streamPlanError}
      </p>
    {/if}
    {#if store.config.experimentalDatagramThroughput || datagramSelected}
      <p class="notice" data-tone="warn" role="status">
        <span
          ><strong>Datagram delivery, not packet loss.</strong> Datagrams are never
          resent; expect lower rates than streams, mostly for uploads.</span
        >
      </p>
    {/if}
  </section>

  <div class="settings-reset">
    <button
      class="btn btn-danger"
      type="button"
      disabled={running || store.preparing}
      onclick={() => (resetConfirmOpen = true)}>Reset settings</button
    >
  </div>
</div>

<ConfirmDialog
  open={resetConfirmOpen}
  id="settings-reset-confirm"
  title="Reset settings?"
  description="Restore test, display and history-saving settings to their defaults? Your theme, panel layout and saved results are kept."
  cancelLabel="Keep settings"
  confirmLabel="Reset settings"
  onCancel={() => (resetConfirmOpen = false)}
  onConfirm={resetSettings}
/>

<style>
  .settings {
    display: grid;
    gap: var(--space-5);
    container: settings / inline-size;
  }
  .group-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }
  /* One row per setting: its name on the left, its control on the right. */
  .row {
    display: flex;
    flex: 1;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-width: 0;
    min-height: var(--control-h);
    font-size: var(--type-sm);
  }
  .switch-row > :global(.switch) {
    flex: 1;
    flex-direction: row-reverse;
    min-height: var(--control-h);
  }
  .value {
    font: var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .number {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-soft);
    font: var(--type-xs) var(--font-mono);
  }
  .row input {
    width: 7rem;
    text-align: end;
  }
  .row select {
    width: auto;
    max-width: 11rem;
  }
  .presets {
    flex: 1;
  }
  .presets > button {
    text-transform: capitalize;
  }
  .row > .segmented {
    flex: 0 1 12rem;
  }
  .settings-reset {
    padding-top: var(--space-3);
    border-top: 1px solid var(--border);
  }
</style>
