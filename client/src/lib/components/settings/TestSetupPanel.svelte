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
    ["warmupMs", phaseLabel("warmup"), "warmup"],
    ["latencyMs", STAGE.latency.label, "latency"],
    ["downloadMs", STAGE.download.label, "download"],
    ["uploadMs", STAGE.upload.label, "upload"],
    ["bidirectionalMs", STAGE.bidirectional.short, "bidirectional"],
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
    <h3>
      <span {@attach tooltip(() => JARGON.stageTime)}>Duration</span>
    </h3>
    <div class="kv">
      <div class="row">
        <span>Preset</span>
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
      <div class="stages" data-count={activeDurationFields.length}>
        {#each activeDurationFields as [key, label, tone] (key)}
          {@const [min, max] = DURATION_LIMITS[key]}
          {@const tip =
            key === "warmupMs"
              ? JARGON.warmup
              : durationMode === "custom"
                ? `${label}\n${fmtDuration(min, 0)} to ${fmtDuration(max)}\nSwitch the stage off under Test stages to skip it`
                : ""}
          {#if durationMode === "custom"}
            <label class="stage" data-tone={tone}>
              <span class="caption"
                ><span {@attach tip ? term(() => tip) : null}>{label}</span>
                <span class="unit">ms</span></span
              >
              <input
                type="number"
                {min}
                {max}
                step="500"
                disabled={store.preparing}
                value={store.config.duration[key]}
                onchange={(event) => setDuration(key, event)}
              />
            </label>
          {:else}
            {@const [value, unit] = fmtDuration(
              DURATION_PRESETS[durationMode][key],
            ).split(" ")}
            <div class="stage" data-tone={tone}>
              <span class="caption"
                ><span {@attach tip ? term(() => tip) : null}>{label}</span>
                <span class="unit">{unit}</span></span
              >
              <span class="value">{value}</span>
            </div>
          {/if}
        {/each}
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
      <div class="row">
        <span {@attach tooltip(() => unitsTip)}>Units</span>
        <div class="controls">
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
              onclick={() => store.prefer({ unitBase: "base10" })}
              >Decimal</button
            >
            <button
              type="button"
              aria-pressed={store.unitBase === "base2"}
              onclick={() => store.prefer({ unitBase: "base2" })}>Binary</button
            >
          </div>
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
        <label class="row">
          <span {@attach tooltip(() => JARGON.gaugeMax)}>Maximum</span>
          <span class="measure">
            <input
              type="number"
              min="1"
              value={vizDisplay}
              onchange={setVizMax}
            />
            <span>{vizUnit}</span>
          </span>
        </label>
      {/if}
    </div>
    {@render rejectedHint("gauge")}
  </section>

  <section class="group">
    <div class="group-head">
      <h3>History</h3>
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
    <h3>Latency probes</h3>
    <div class="kv">
      {#each CADENCES as [key, label, tip] (key)}
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
      <label class="row">
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
  .row {
    justify-content: space-between;
  }
  .row input {
    width: 6rem;
    text-align: end;
  }
  .row select {
    width: auto;
    max-width: 11rem;
  }
  .presets > button {
    text-transform: capitalize;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .stages {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--space-3) var(--space-2);
    padding-block: var(--space-2);
  }
  .stages[data-count="5"] {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
  @container settings (min-width: 420px) {
    .stages[data-count="5"] {
      grid-template-columns: repeat(5, minmax(0, 1fr));
    }
  }
  .stage {
    display: grid;
    gap: 2px;
    min-width: 0;
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
  .stage::before {
    content: "";
    height: 3px;
    margin-bottom: 6px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  .caption {
    display: flex;
    justify-content: space-between;
    gap: var(--space-1);
  }
  .unit {
    font-size: var(--type-2xs);
  }
  /* A preset time sits where its custom field goes, so switching moves nothing. */
  .value {
    height: var(--control-h);
    color: var(--text);
    font-size: var(--type-body);
    line-height: var(--control-h);
  }
  @media (pointer: coarse) {
    .value {
      height: var(--hit);
      line-height: var(--hit);
    }
  }
  .stage input {
    appearance: textfield;
  }
  .stage input::-webkit-inner-spin-button {
    appearance: none;
  }
  .measure {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
  .segmented > button {
    flex: 1 0 auto;
  }
  .settings-reset {
    margin-top: var(--space-1);
  }
</style>
