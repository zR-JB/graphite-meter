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
  import Switch from "../Switch.svelte";
  import ServerSelection from "../ServerSelection.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";
  import {
    JARGON,
    phaseLabel,
    PING_CADENCE,
    READINESS,
    STAGE,
  } from "../../presentation/vocabulary";
  import { fmtDuration } from "../../format";
  import { untrack } from "svelte";
  import {
    announce,
    announceChanges,
  } from "../../presentation/announcer.svelte";
  import ConfirmDialog from "../ConfirmDialog.svelte";

  let { onOpenHistory }: { onOpenHistory: (invoker: HTMLElement) => void } =
    $props();
  const running = $derived(store.isRunning);
  let resetConfirmOpen = $state(false);
  $effect(() => {
    if (
      store.serverCatalog &&
      !store.catalogLoading &&
      !store.isRunning &&
      !store.preparing
    ) {
      untrack(() => controller.loadServerMetadata());
      return () => controller.cancelServerMetadata();
    }
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
    ["pingCadence", "Idle latency cadence"],
    ["loadedPingCadence", "Loaded latency cadence"],
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
  const presetCells = $derived.by(() => {
    const preset = durationMode;
    if (preset === "custom") return [];
    return activeDurationFields.map(([key, label]) => ({
      label,
      value: fmtDuration(DURATION_PRESETS[preset][key]),
    }));
  });

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

  const readiness = $derived(store.selectionValidation);
  announceChanges(() => `Connection paths: ${READINESS[readiness].label}`);
</script>

{#snippet rejectedHint(field: typeof rejected)}
  {#if rejected === field}<p class="notice" data-tone="warn">
      {rejection}
    </p>{/if}
{/snippet}

<div class="setup-grid">
  <h2 class="caps tier-label">Test</h2>
  <section class="surface-inset panel wide primary">
    <div class="section-heading">
      <h3 class="caps">Connection paths</h3>
      <span
        class="badge term"
        data-readiness={readiness}
        data-tone={READINESS[readiness].tone}
        {@attach tooltip(() =>
          readiness === "verified"
            ? JARGON.checkReuse
            : READINESS[readiness].label,
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
  <section class="surface-inset panel">
    <h3 class="caps">Duration &amp; stages</h3>
    <div class="segmented presets" role="group" aria-label="Duration preset">
      {#each PRESETS as preset}
        <button
          type="button"
          aria-pressed={durationMode === preset}
          disabled={store.preparing}
          onclick={() => setPreset(preset)}>{preset}</button
        >
      {/each}
    </div>
    <Switch
      checked={store.config.stages.bidirectional}
      onToggle={setBidirectional}
      disabled={store.preparing ||
        (running && store.phaseStage === "bidirectional")}
      label="Include concurrent download + upload"
    />
    {#if durationMode === "custom"}
      <div class="duration-fields">
        {#each activeDurationFields as [key, label]}
          <label class="field">
            <span>{label} ms</span>
            <input
              type="number"
              min="0"
              max={DURATION_LIMITS[key][1]}
              step="500"
              disabled={store.preparing}
              value={store.config.duration[key]}
              onchange={(event) => setDuration(key, event)}
            />
          </label>
        {/each}
      </div>
      <p class="hint">Stages run 1 s to 5 min; 0 skips a stage.</p>
    {:else}
      <div class="dur-summary">
        {#each presetCells as cell}
          <div class="dur-cell">
            <span class="caps">{cell.label}</span>
            <strong>{cell.value}</strong>
          </div>
        {/each}
      </div>
    {/if}
    {@render rejectedHint("duration")}
    {#if running}
      <p class="hint">
        Active and future durations, plus unstarted stages, update this run.
      </p>
    {/if}
  </section>
  <h2 class="caps tier-label">Results</h2>
  <section class="surface-inset panel">
    <h3 class="caps">Display units</h3>
    <div class="two">
      <div class="field">
        <span>Rate</span>
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
      <div class="field">
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
    </div>
    <p class="hint">Applies to all displayed rates.</p>
  </section>
  <section class="surface-inset panel wide">
    <h3 class="caps">Result history</h3>
    <Switch
      checked={store.savingResults}
      onToggle={(enabled) =>
        store.prefer({
          resultHistoryPreference: enabled ? "enabled" : "disabled",
        })}
      label="Save completed results on this device"
    />
    <a
      class="btn-link"
      href="#/history"
      onclick={(event) => {
        event.preventDefault();
        onOpenHistory(event.currentTarget as HTMLElement);
      }}>View History</a
    >
  </section>
  <section class="surface-inset panel wide">
    <h3 class="caps">Wire-rate estimates</h3>
    <Switch
      checked={store.showWireEstimates}
      onToggle={(showWireEstimates) => store.prefer({ showWireEstimates })}
      label="Show estimated wire rate"
      tooltip={JARGON.wireRate}
    />
    <p class="hint">
      Estimated Ethernet rate from measured protocol bytes and available
      connection details.
    </p>
  </section>
  <section class="surface-inset panel">
    <h3 class="caps">Gauge scale</h3>
    <Switch
      checked={vizAuto}
      onToggle={setVizAuto}
      label="Scale throughput automatically"
    />
    {#if !vizAuto}
      <label class="field">
        <span>Maximum {store.unitLabel}</span>
        <input
          type="number"
          min="1"
          value={Number(vizDisplay.toFixed(2))}
          onchange={setVizMax}
        />
      </label>
    {/if}
    {@render rejectedHint("gauge")}
    <p class="hint">
      {#if vizAuto}
        The chart follows the measured peak. The gauge starts at 1 Gbit/s and
        grows in powers of ten.
      {:else}
        Sets the chart ceiling; the gauge rounds up to a readable scale.
      {/if}
    </p>
  </section>
  <h2 class="caps tier-label">Advanced</h2>
  <section class="surface-inset panel">
    <h3 class="caps">Early finish</h3>
    <Switch
      checked={store.config.adaptive}
      onToggle={(adaptive) => controller.configureRun({ adaptive })}
      disabled={store.preparing}
      label="Finish stable stages early"
    />
  </section>
  <section class="surface-inset panel">
    <h3 class="caps">Latency timing</h3>
    {#each CADENCES as [key, label] (key)}
      <label class="field">
        <span>{label}</span>
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
    {/each}
    <Switch
      checked={store.config.skipLoadedLatencyWhenStageOff}
      onToggle={(skipLoadedLatencyWhenStageOff) =>
        controller.configureRun({ skipLoadedLatencyWhenStageOff })}
      disabled={running || store.preparing}
      label="Skip loaded latency when latency is off"
    />
  </section>
  <section class="surface-inset panel">
    <h3 class="caps">Datagram throughput</h3>
    {#if store.config.experimentalDatagramThroughput || datagramSelected}
      <!-- Above the toggle, where a long scroll ends; a status, not an alert. -->
      <p class="notice" data-tone="warn" role="status">
        <strong>Measures application datagram delivery.</strong> Datagrams are not
        retransmitted. Missing deliveries can come from network or endpoint queues;
        they do not identify physical packet loss. Expect a lower received rate than
        stream transfers, especially for browser uploads.
      </p>
    {/if}
    <Switch
      checked={store.config.experimentalDatagramThroughput}
      onToggle={(experimentalDatagramThroughput) =>
        controller.configureRun({ experimentalDatagramThroughput })}
      disabled={running || store.preparing}
      label="Datagram throughput (experimental)"
    />
    <p class="hint">
      Adds the WebTransport datagram card to the connection picker.
    </p>
  </section>
  <section class="surface-inset panel">
    <h3 class="caps">Transfer streams</h3>
    <Switch
      checked={store.config.transferStreams.mode === "forced"}
      onToggle={(forced) => streams({ mode: forced ? "forced" : "auto" })}
      disabled={running || store.preparing}
      label="Force exact stream count"
      tooltip={JARGON.forcedStreams}
    />
    <label class="field">
      <span
        >{store.config.transferStreams.mode === "forced"
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
    {@render rejectedHint("streams")}
    {#if store.streamPlanError}
      <p class="notice" data-tone="warn" role="status">
        {store.streamPlanError}
      </p>
    {/if}
    {#if store.config.transferStreams.mode === "forced"}
      <p class="hint">
        Starts exactly {store.config.transferStreams.count} requests per server and
        active direction. The run reserves progress and control capacity and allows
        at most 128 streams per direction.
      </p>
    {:else}
      <p class="hint">
        Automatic caps HTTP/1.1 at {store.config.transferStreams.count}. HTTP/2
        and HTTP/3 choose safe multiplexed request counts automatically.
      </p>
    {/if}
  </section>
  <div class="settings-reset wide">
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
  description={JARGON.resetSettings}
  cancelLabel="Keep settings"
  confirmLabel="Reset settings"
  onCancel={() => (resetConfirmOpen = false)}
  onConfirm={resetSettings}
/>

<style>
  .setup-grid {
    display: grid;
    /* Connection cards share this breakpoint so a widened dock reflows as one. */
    --settings-card-min: 180px;
    grid-template-columns: repeat(
      auto-fit,
      minmax(min(100%, var(--settings-card-min)), 1fr)
    );
    gap: var(--space-3);
    container: settings-grid / inline-size;
  }
  .panel {
    display: grid;
    align-content: start;
    gap: var(--space-3);
    min-width: 0;
    padding: var(--space-3);
  }
  .wide,
  .tier-label {
    grid-column: 1 / -1;
  }
  .primary {
    border-color: color-mix(in srgb, var(--brand) 24%, var(--border));
  }
  .tier-label {
    margin-top: var(--space-1);
  }
  .tier-label:first-child {
    margin-top: 0;
  }
  .section-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .presets > button {
    text-transform: capitalize;
  }
  .btn-link {
    justify-self: start;
  }
  .settings-reset {
    padding-top: var(--space-3);
    border-top: 1px solid var(--border);
  }
  .two,
  .duration-fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: var(--space-2) var(--space-3);
  }
  .dur-summary {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 70px), 1fr));
    gap: 6px;
  }
  .dur-cell {
    display: grid;
    gap: 2px;
    min-width: 0;
    padding: 6px var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-1);
  }
  .dur-cell span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dur-cell strong {
    font: var(--w-strong) var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
</style>
