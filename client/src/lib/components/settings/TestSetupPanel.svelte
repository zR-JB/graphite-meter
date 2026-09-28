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
  import {
    BROWSER_CONNECTION_BUDGET,
    normalizeStreamCount,
  } from "../../runner/paths";
  import { term, tooltip } from "../../actions/tooltip";
  import { reveal } from "../../presentation/motion.svelte";
  import Icon from "../Icon.svelte";
  import Switch from "../Switch.svelte";
  import ServerSelection from "../ServerSelection.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";
  import DurationStrip from "./DurationStrip.svelte";
  import {
    BLOCKED,
    JARGON,
    phaseLabel,
    PING_CADENCE,
    READINESS,
    READINESS_TIP,
    STAGE,
  } from "../../presentation/vocabulary";
  import {
    fmtDuration,
    rateUnit,
    rateValueAt,
    rawRateFrom,
  } from "../../format";
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
      selectedServers,
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
    ["warmupMs", phaseLabel("warmup"), "warmup"],
    ["latencyMs", STAGE.latency.label, "latency"],
    ["downloadMs", STAGE.download.label, "download"],
    ["uploadMs", STAGE.upload.label, "upload"],
    ["bidirectionalMs", STAGE.bidirectional.short, "bidirectional"],
  ] as const;
  type DurationKey = (typeof DURATION_FIELDS)[number][0];
  const WARMUP_LABEL = DURATION_FIELDS[0][1];
  const STAGE_FIELDS = DURATION_FIELDS.slice(1);
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
  // Stage times move in half seconds; warmup, a fraction of a second, in tenths.
  const STEP_MS: Record<DurationKey, number> = {
    warmupMs: 100,
    latencyMs: 500,
    downloadMs: 500,
    uploadMs: 500,
    bidirectionalMs: 500,
  };
  function applyDuration(key: DurationKey, value: number): boolean {
    const current = store.config.duration[key];
    const accepted =
      value === current ||
      controller.configureRun({
        duration: { ...store.config.duration, [key]: value },
      });
    rejected = accepted ? null : "duration";
    if (!accepted) announce(rejection);
    return accepted;
  }
  // A stage stays within what every selected server admits.
  const maxMs = (key: DurationKey) =>
    key === "warmupMs"
      ? DURATION_LIMITS.warmupMs[1]
      : Math.min(DURATION_LIMITS[key][1], store.stageLimit.ms);
  const limited = (key: DurationKey, ms: number) =>
    Math.min(maxMs(key), clampDuration(key, ms));
  function nudge(key: DurationKey, delta: number) {
    applyDuration(key, limited(key, store.config.duration[key] + delta));
  }
  function setSeconds(key: DurationKey, event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const step = STEP_MS[key];
    const raw = input.valueAsNumber;
    const value = Number.isFinite(raw)
      ? limited(key, Math.round((raw * 1000) / step) * step)
      : store.config.duration[key];
    const accepted = applyDuration(key, value);
    input.value = String(
      (accepted ? value : store.config.duration[key]) / 1000,
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
    return [
      name,
      ...activeDurationFields.map(
        ([key, label]) => `${label} ${fmtDuration(times[key])}`,
      ),
    ].join("\n");
  }
  const unitsTip = [
    "Units",
    ...[JARGON.rateUnit, JARGON.unitPrefix].flatMap((tip) =>
      tip.split("\n").slice(1),
    ),
  ].join("\n");

  const forced = $derived(store.config.transferStreams.mode === "forced");
  const queuedStreams = $derived(
    forced &&
      store.config.transferStreams.count > BROWSER_CONNECTION_BUDGET &&
      store.selectedServers.some((id) => {
        const path = store.servers.get(id)?.paths?.throughput;
        return (
          path?.target.transport === "fetch-stream" &&
          path.fetch.protocol === "http1"
        );
      }),
  );
  const streams = (patch: Partial<RunnerConfig["transferStreams"]>) =>
    controller.configureRun({
      transferStreams: { ...store.config.transferStreams, ...patch },
    });
  const gaugeMax = (throughputMaxBytesPerSec: number | "auto") =>
    controller.configureRun({ visualization: { throughputMaxBytesPerSec } });

  const vizAuto = $derived(
    store.config.visualization.throughputMaxBytesPerSec === "auto",
  );
  // The maximum keeps one prefix, so a typed number never changes meaning.
  const MAX_UNIT = 2;
  const vizUnit = $derived(rateUnit(store.unitBase, store.unitKind, MAX_UNIT));
  const vizDisplay = $derived(
    vizAuto
      ? 0
      : Number(
          rateValueAt(
            store.config.visualization.throughputMaxBytesPerSec as number,
            store.unitBase,
            store.unitKind,
            MAX_UNIT,
          ).toPrecision(4),
        ),
  );
  function setVizAuto(auto: boolean) {
    gaugeMax(
      auto ? "auto" : Math.max(1, Math.round(store.scales.chartBytesPerSec)),
    );
  }
  function setVizMax(event: Event) {
    commitNumber(
      event,
      "gauge",
      vizDisplay,
      (value) => (value > 0 ? value : vizDisplay),
      (value) =>
        gaugeMax(
          Math.max(
            1,
            Math.round(
              rawRateFrom(value, store.unitBase, store.unitKind, MAX_UNIT),
            ),
          ),
        ),
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

{#snippet stepper(key: DurationKey, label: string)}
  {@const min = DURATION_LIMITS[key][0]}
  {@const max = maxMs(key)}
  {@const ms = store.config.duration[key]}
  <span class="stepper" role="group" aria-label="{label} time">
    <button
      type="button"
      class="btn btn-icon btn-quiet"
      aria-label="Shorter {label}"
      disabled={store.preparing || ms <= min}
      onclick={() => nudge(key, -STEP_MS[key])}>−</button
    >
    <input
      type="number"
      min={min / 1000}
      max={max / 1000}
      step={STEP_MS[key] / 1000}
      disabled={store.preparing}
      value={ms / 1000}
      aria-label="{label} in seconds"
      onchange={(event) => setSeconds(key, event)}
    />
    <span class="unit">s</span>
    <button
      type="button"
      class="btn btn-icon btn-quiet"
      aria-label="Longer {label}"
      disabled={store.preparing || ms >= max}
      onclick={() => nudge(key, STEP_MS[key])}>+</button
    >
  </span>
{/snippet}

{#snippet toggle(
  label: string,
  tip: string,
  checked: boolean,
  onToggle: (next: boolean) => void,
  disabled = false,
)}
  <Switch {checked} {onToggle} {disabled} {label} tooltip={tip} />
{/snippet}

<div class="settings">
  <section class="group">
    <div class="group-head">
      <h3>Connection</h3>
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
    <div class="group-head">
      <h3><span {@attach tooltip(() => JARGON.stageTime)}>Duration</span></h3>
      <span class="aside">{fmtDuration(store.totalEtaMs, 0)} in total</span>
    </div>
    <div class="kv">
      <div class="presets">
        <div class="segmented" role="group" aria-label="Duration preset">
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
      <div class="strip-row">
        <DurationStrip
          stages={STAGE_FIELDS.filter(
            ([key]) =>
              key !== "bidirectionalMs" || store.config.stages.bidirectional,
          ).map(([key, label, tone]) => ({
            key,
            label,
            tone,
            ms: store.config.duration[key],
          }))}
        />
      </div>
      {#if durationMode === "custom"}
        {#each STAGE_FIELDS as [key, label, tone] (key)}
          {#if key !== "bidirectionalMs" || store.config.stages.bidirectional}
            <div class="stage-row" transition:reveal|global>
              <span class="stage-name" data-tone={tone}>{label}</span>
              {@render stepper(key, label)}
            </div>
          {/if}
        {/each}
      {/if}
      <div class="stage-row">
        <span {@attach term(() => JARGON.warmup)}>{WARMUP_LABEL}</span>
        {#if durationMode === "custom"}
          {@render stepper("warmupMs", WARMUP_LABEL)}
        {:else}
          <span class="value"
            >{fmtDuration(store.config.duration.warmupMs)}</span
          >
        {/if}
      </div>
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
    <h3>Display</h3>
    <div class="kv">
      <div class="units">
        <span {@attach tooltip(() => unitsTip)}>Units</span>
        <div class="segmented" role="group" aria-label="Rate unit">
          <button
            type="button"
            aria-pressed={store.unitKind === "bits"}
            onclick={() => store.prefer({ unitKind: "bits" })}>Bits</button
          >
          <button
            type="button"
            aria-pressed={store.unitKind === "bytes"}
            onclick={() => store.prefer({ unitKind: "bytes" })}>Bytes</button
          >
        </div>
        <div class="segmented" role="group" aria-label="Prefix scale">
          <button
            type="button"
            aria-pressed={store.unitBase === "base10"}
            onclick={() => store.prefer({ unitBase: "base10" })}>Decimal</button
          >
          <button
            type="button"
            aria-pressed={store.unitBase === "base2"}
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
        <label transition:reveal>
          <span {@attach tooltip(() => JARGON.gaugeMax)}>Maximum</span>
          <span class="field-unit">
            <input
              type="number"
              min="1"
              value={vizDisplay}
              onchange={setVizMax}
            />
            <span class="unit">{vizUnit}</span>
          </span>
        </label>
      {/if}
    </div>
    {@render rejectedHint("gauge")}
  </section>

  <section class="group">
    <h3>History</h3>
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
      <a
        class="link-row"
        href="#/history"
        onclick={(event) => {
          event.preventDefault();
          onOpenHistory(event.currentTarget as HTMLElement);
        }}>Open History<Icon name="chevron" /></a
      >
    </div>
  </section>

  <section class="group">
    <h3>Latency probes</h3>
    <div class="kv">
      {#each CADENCES as [key, label, tip] (key)}
        <label>
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
    <h3>Transfers</h3>
    <div class="kv">
      {@render toggle(
        "Force exact stream count",
        JARGON.forcedStreams,
        forced,
        (on) => streams({ mode: on ? "forced" : "auto" }),
        running || store.preparing,
      )}
      <label>
        <span
          {@attach term(() =>
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
    {#if queuedStreams}
      <p class="hint">
        Browsers run {BROWSER_CONNECTION_BUDGET} HTTP/1.1 requests per server at once.
        Streams past that wait for a free connection.
      </p>
    {/if}
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

  <button
    class="btn btn-danger reset"
    type="button"
    disabled={running || store.preparing}
    onclick={() => (resetConfirmOpen = true)}>Reset settings</button
  >
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
  }
  .presets {
    padding-block: var(--space-3) 0;
  }
  .presets .segmented {
    flex: 1;
  }
  .presets button {
    flex: 1 1 0;
    text-transform: capitalize;
  }
  .kv > .presets + .strip-row {
    border-top: 0;
  }
  .strip-row {
    display: block;
  }
  .stage-name {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
  .stage-name::before {
    content: "";
    width: 7px;
    height: 7px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  .stage-row {
    align-items: center;
  }
  .stage-row .value {
    font-variant-numeric: tabular-nums;
  }
  /* − time + : the field keeps its value editable, the buttons step it. */
  .stepper {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r-chrome);
    background: var(--track);
  }
  .stepper .btn {
    --control-h: 28px;
    width: 28px;
    font: var(--w-normal) var(--type-lg) / 1 var(--font-sans);
  }
  .kv .stepper input {
    width: 3.5rem;
    height: 28px;
    padding: 0;
    border: 0;
    background: none;
    box-shadow: none;
    font-variant-numeric: tabular-nums;
    text-align: end;
  }
  .unit {
    margin-inline: 2px 4px;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1 var(--font-sans);
  }
  .kv input[type="number"] {
    width: 5.5rem;
    text-align: end;
    appearance: textfield;
  }
  .kv input::-webkit-inner-spin-button {
    appearance: none;
  }
  .field-unit {
    display: flex;
    flex-wrap: wrap;
    align-content: center;
    align-items: baseline;
    width: 7rem;
    height: var(--control-h);
    padding-inline: 10px;
    border: 1px solid var(--field-edge);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    transition: var(--transition-control);
  }
  .field-unit:has(input:focus-visible) {
    border-color: var(--brand-line);
    box-shadow: var(--ring-halo);
  }
  .field-unit:has(input:disabled) {
    opacity: 0.5;
  }
  .kv .field-unit input {
    flex: 1;
    width: 0;
    height: auto;
    padding: 0;
    border: 0;
    background: none;
    box-shadow: none;
  }
  @media (pointer: coarse) {
    .field-unit {
      height: var(--hit);
    }
  }
  .units {
    column-gap: var(--space-2);
  }
  .units > span {
    margin-inline-end: auto;
  }
  .kv select {
    width: auto;
    max-width: 11rem;
  }
  .reset {
    justify-self: start;
  }
</style>
