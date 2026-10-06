<script lang="ts">
  import { catalogSelection } from "../../presentation/serverAppearance";
  import { store } from "../../state/store.svelte";
  import { DURATION_LIMITS, DURATION_PRESETS } from "../../state/defaults";
  import type { PingCadence, RunnerConfig } from "../../runner/contract";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  import { pathOptions } from "../../presentation/paths";
  import {
    BROWSER_CONNECTION_BUDGET,
    normalizeStreamCount,
  } from "../../runner/paths";
  import { tooltip } from "../../actions/tooltip";
  import { flip, reveal } from "../../presentation/motion.svelte";
  import Icon from "../Icon.svelte";
  import Switch from "../Switch.svelte";
  import ServerSelection from "../ServerSelection.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";
  import DurationStrip from "./DurationStrip.svelte";
  import Stepper from "./Stepper.svelte";
  import Roll from "../Roll.svelte";
  import {
    BLOCKED,
    JARGON,
    phaseLabel,
    PING_CADENCE,
    PING_CADENCE_SHORT,
    READINESS,
    READINESS_TIP,
    STAGE,
  } from "../../presentation/vocabulary";
  import {
    fmtDuration,
    fmtStageTime,
    parseDuration,
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
  const [, ...STAGE_FIELDS] = DURATION_FIELDS;
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
  // Escape drops a typed number; the sheet closes on the next one. Bound as a
  // capture listener on the field, so it runs before the sheet's own.
  function keepOnEscape(event: KeyboardEvent, current: number) {
    const input = event.currentTarget as HTMLInputElement;
    if (event.key !== "Escape" || input.value === String(current)) return;
    input.value = String(current);
    event.preventDefault();
  }
  const rejection = $derived(
    store.startError || "This change cannot apply to the current run.",
  );
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
  // Warmup moves in tenths; a stage's step grows with it, so the buttons reach an hour as readily as a second.
  function stepMs(key: DurationKey, ms: number, direction: 1 | -1) {
    if (key === "warmupMs") return 100;
    const from = direction < 0 ? ms - 1 : ms;
    if (from < 10_000) return 500;
    if (from < 60_000) return 1_000;
    if (from < 600_000) return 10_000;
    return from < 3_600_000 ? 60_000 : 300_000;
  }
  function setBidirectional(enabled: boolean) {
    flip(() =>
      controller.configureRun({
        stages: { ...store.config.stages, bidirectional: enabled },
      }),
    );
  }
  const activeDurationFields = $derived(
    store.config.stages.bidirectional
      ? DURATION_FIELDS
      : DURATION_FIELDS.filter(([key]) => key !== "bidirectionalMs"),
  );
  const presetName = (preset: Preset) =>
    preset[0].toUpperCase() + preset.slice(1);
  function presetTip(preset: Preset) {
    const name = presetName(preset);
    if (preset === "custom") return `${name}\nSet each stage's time`;
    const times = DURATION_PRESETS[preset];
    return [
      name,
      ...activeDurationFields.map(
        ([key, label]) => `${label} ${fmtStageTime(times[key])}`,
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
  // A typed stream count is a whole number.
  const parseCount = (text: string) => {
    const count = Number(text.trim());
    return text.trim() && Number.isInteger(count) ? count : null;
  };
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
      ? `Connection: ${READINESS[readiness].label}`
      : "",
  );
</script>

{#snippet rejectedHint(field: typeof rejected)}
  {#if rejected === field}<p class="notice" data-tone="warn">
      {rejection}
    </p>{/if}
{/snippet}

{#snippet stepper(key: DurationKey, label: string)}
  <Stepper
    {label}
    value={store.config.duration[key]}
    min={DURATION_LIMITS[key][0]}
    max={maxMs(key)}
    step={(ms, direction) => stepMs(key, ms, direction)}
    format={fmtStageTime}
    parse={parseDuration}
    unit={1000}
    verbs={["Shorten", "Lengthen"]}
    noun="time"
    disabled={store.preparing}
    onChange={(ms) => applyDuration(key, ms)}
  />
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
      <span class="aside"
        ><Roll
          text={fmtDuration(store.totalEtaMs, 0)}
          rank={store.totalEtaMs}
        /> in total</span
      >
    </div>
    <div class="kv">
      <div class="presets">
        <div class="segmented" role="group" aria-label="Duration preset">
          {#each PRESETS as preset (preset)}
            <button
              type="button"
              aria-pressed={durationMode === preset}
              disabled={store.preparing}
              {@attach tooltip(() => presetTip(preset))}
              onclick={() => setPreset(preset)}>{presetName(preset)}</button
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
        {#each STAGE_FIELDS as [key, , tone] (key)}
          {#if key !== "bidirectionalMs" || store.config.stages.bidirectional}
            <div class="stage-row" transition:reveal|global>
              <span class="stage-name" data-tone={tone}
                >{STAGE[tone].label}</span
              >
              {@render stepper(key, `${STAGE[tone].label} stage`)}
            </div>
          {/if}
        {/each}
      {/if}
      <div class="stage-row">
        <span
          class="stage-name"
          data-tone="warmup"
          {@attach tooltip(() => JARGON.warmup)}>{WARMUP_LABEL}</span
        >
        {#if durationMode === "custom"}
          {@render stepper("warmupMs", "warmup")}
        {:else}
          <Roll
            text={fmtStageTime(store.config.duration.warmupMs)}
            rank={store.config.duration.warmupMs}
          />
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
    {#if store.stageLimitError}<p class="notice" data-tone="warn" role="status">
        {store.stageLimitError}
      </p>{/if}
    {@render rejectedHint("duration")}
    {#if running}
      <p class="hint">Changes apply to the current and upcoming stages.</p>
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
              onkeydowncapture={(event) => keepOnEscape(event, vizDisplay)}
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
        "Save results in this browser",
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
        <div class="cadence stacked">
          <span {@attach tooltip(() => tip)}>{label}</span>
          <div class="segmented" role="group" aria-label={label}>
            {#each Object.keys(PING_CADENCE) as PingCadence[] as value (value)}
              <button
                type="button"
                aria-pressed={store.config[key] === value}
                disabled={running || store.preparing}
                {@attach tooltip(() => PING_CADENCE[value])}
                onclick={() => controller.configureRun({ [key]: value })}
                >{PING_CADENCE_SHORT[value]}</button
              >
            {/each}
          </div>
        </div>
      {/each}
      {@render toggle(
        "Skip loaded latency if the Latency stage is off",
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
      <div class="units">
        <span
          {@attach tooltip(() =>
            forced ? JARGON.forcedStreamCount : JARGON.autoStreamCount,
          )}
          >{forced
            ? "Streams per server and direction"
            : "HTTP/1.1 stream limit per direction"}</span
        >
        <Stepper
          label={forced ? "streams" : "stream limit"}
          value={store.config.transferStreams.count}
          min={1}
          max={128}
          step={() => 1}
          format={String}
          parse={parseCount}
          verbs={["Fewer", "More"]}
          fieldMin="3ch"
          disabled={running || store.preparing}
          onChange={(count) => {
            const accepted = streams({ count: normalizeStreamCount(count) });
            if (!accepted) announce(rejection);
            return accepted;
          }}
        />
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
  description="Test, display and history-saving settings return to their defaults. Your theme, panel layout and saved results are kept."
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
  .group-head > .aside {
    color: var(--text-soft);
    font: var(--role-label);
    font-variant-numeric: tabular-nums;
  }
  .presets {
    padding-block: var(--space-3) 0;
  }
  .presets .segmented {
    flex: 1;
  }
  .presets button {
    flex: 1 1 0;
    min-width: max-content;
  }
  .kv > .presets + .strip-row {
    border-top: 0;
  }
  /* The strip keeps its own spacing. */
  .strip-row {
    display: block;
    padding-block: 0;
  }
  .stage-row {
    align-items: center;
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
  /* A control stands at its label's end while the row holds both, and against the right edge when it wraps
     under the label; a cadence's segments then take the row's width. */
  .units > span {
    flex: 1 0 auto;
    margin-inline-end: auto;
  }
  .units > :not(span) {
    margin-inline-start: auto;
  }
  /* A narrow sheet stacks every stepper row and the units row alike, so no row wraps where its neighbour does not. */
  @container settings (max-width: 320px) {
    .stage-row:has(:global(.stepper)) {
      flex-flow: column nowrap;
      align-items: flex-start;
      gap: var(--space-2);
      padding-block: var(--space-3);
    }
    .units {
      row-gap: var(--space-2);
      padding-block: var(--space-3);
    }
    .units > span {
      flex-basis: 100%;
    }
  }
  .reset {
    justify-self: start;
  }
</style>
