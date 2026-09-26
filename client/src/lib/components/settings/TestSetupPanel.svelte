<script lang="ts">
  import { store } from "../../state/store.svelte";
  import { DURATION_PRESETS } from "../../state/defaults";
  import type { ProtocolTarget, RunnerConfig } from "../../runner/contract";
  import type {
    FetchThroughputTarget,
    LatencyTarget,
    WebTransportThroughputTarget,
  } from "../../api/endpoints";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  import { describeTarget } from "../../runner/real/targetPresentation";
  import { normalizeStreamCount } from "../../runner/real/streamPolicy";
  import { panelReadiness } from "../../runner/connectionModel";
  import { JARGON, tooltip } from "../../actions/tooltip";
  import Switch from "../Switch.svelte";
  import { serverTransportOptions } from "../../servers/transportOptions";
  import ServerSelection from "../ServerSelection.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";
  import { PING_CADENCE } from "../../presentation/vocabulary";
  import { fmtDuration } from "../../format";
  import { untrack } from "svelte";
  import ConfirmDialog from "../ConfirmDialog.svelte";

  interface Props {
    open: boolean;
    onOpenHistory: (invoker: HTMLElement) => void;
  }
  let { open, onOpenHistory }: Props = $props();
  const running = $derived(store.isRunning);
  let resetConfirmOpen = $state(false);
  $effect(() => {
    if (!open) resetConfirmOpen = false;
  });
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
  function resetSettings() {
    resetConfirmOpen = false;
    store.restoreTestDisplayDefaults();
    customDuration = false;
  }

  function targetOption(
    target:
      FetchThroughputTarget | WebTransportThroughputTarget | LatencyTarget,
    observedProtocol?: ProtocolTarget,
  ) {
    return {
      value: target.id,
      label: describeTarget(store.transportDiscovery!, target, observedProtocol)
        .label,
    };
  }
  // The caution belongs to the selection, not the toggle: turning the toggle
  // off keeps a selected datagram card, so it must keep its warning too.
  const datagramSelected = $derived(
    store.connections.throughput.target?.transport === "webtransport-datagram",
  );
  // One card per mechanism an origin advertises. The datagram path is the one
  // gated on its setting, and stays visible while it is the current selection.
  const simultaneous = $derived(store.selectedServers.length > 1);
  const selectedServers = $derived(
    store.serverCatalog?.servers.filter((server) =>
      store.selectedServers.includes(server.id),
    ) ?? [],
  );
  const globalThroughput = $derived(
    store.config.transports.throughputTarget.startsWith("protocol:") ||
      store.config.transports.throughputTarget.startsWith("transport:"),
  );
  const globalLatency = $derived(
    store.config.transports.latencyTarget.startsWith("transport:"),
  );
  const throughputTargets = $derived(
    simultaneous
      ? serverTransportOptions(
          "throughput",
          selectedServers,
          store.servers,
          store.config.experimentalDatagramThroughput,
          store.config.transports.throughputTarget,
        )
      : [
          { value: "auto", label: "Automatic" },
          ...(globalThroughput
            ? serverTransportOptions(
                "throughput",
                selectedServers,
                store.servers,
                store.config.experimentalDatagramThroughput,
                store.config.transports.throughputTarget,
              ).filter(
                (option) =>
                  option.value === store.config.transports.throughputTarget,
              )
            : []),
          ...Object.values(store.transportDiscovery?.throughput ?? {}).flatMap(
            (entry) =>
              entry.targets
                .filter(
                  (target) =>
                    target.transport !== "webtransport-datagram" ||
                    store.config.experimentalDatagramThroughput ||
                    store.config.transports.throughputTarget === target.id,
                )
                .map((target) =>
                  // The observed protocol only describes the path actually in use.
                  targetOption(
                    target,
                    store.connections.throughput.target?.id === target.id
                      ? store.connections.throughput.observedProtocol
                      : undefined,
                  ),
                ),
          ),
        ],
  );
  const latencyTargets = $derived(
    simultaneous
      ? serverTransportOptions(
          "latency",
          store.latencySelection.mode === "primary"
            ? selectedServers.filter(
                (server) => server.id === store.primaryLatencyServer,
              )
            : selectedServers,
          store.servers,
          false,
          store.config.transports.latencyTarget,
        )
      : [
          { value: "auto", label: "Automatic" },
          ...(globalLatency
            ? serverTransportOptions(
                "latency",
                selectedServers,
                store.servers,
                false,
                store.config.transports.latencyTarget,
              ).filter(
                (option) =>
                  option.value === store.config.transports.latencyTarget,
              )
            : []),
          ...Object.values(store.transportDiscovery?.latency ?? {}).flatMap(
            (entry) => entry.targets.map((target) => targetOption(target)),
          ),
        ],
  );

  const CADENCES = [
    ["pingCadence", "Idle latency cadence"],
    ["loadedPingCadence", "Loaded latency cadence"],
  ] as const;
  type Preset = "short" | "medium" | "long" | "custom";
  const PRESETS: Preset[] = ["short", "medium", "long", "custom"];
  const DURATION_FIELDS = [
    ["warmupMs", "Warmup"],
    ["latencyMs", "Latency"],
    ["downloadMs", "Download"],
    ["uploadMs", "Upload"],
    ["bidirectionalMs", "Bidirectional"],
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
  function commitNumber(
    event: Event,
    current: number,
    normalize: (value: number) => number,
    commit: (value: number) => void,
  ) {
    const input = event.currentTarget as HTMLInputElement;
    const raw = input.valueAsNumber;
    const value = Number.isFinite(raw) ? normalize(raw) : current;
    input.value = String(value);
    if (value !== current) commit(value);
  }
  function setDuration(key: DurationKey, event: Event) {
    commitNumber(
      event,
      store.config.duration[key],
      (value) => Math.max(0, value),
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
  function setAdaptiveEnabled(enabled: boolean) {
    controller.configureRun({
      adaptive: { ...store.config.adaptive, enabled },
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

  function setForcedStreams(forced: boolean) {
    store.config.transferStreams.mode = forced ? "forced" : "auto";
  }

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
    store.config.visualization.throughputMaxBytesPerSec = auto
      ? "auto"
      : Math.max(1, Math.round(store.chartScaleBytesPerSec));
  }
  function setVizMax(event: Event) {
    const current = Number(vizDisplay.toFixed(2));
    commitNumber(
      event,
      current,
      (value) => (value > 0 ? value : current),
      (value) =>
        (store.config.visualization.throughputMaxBytesPerSec = Math.max(
          1,
          Math.round(store.fromUnit(value)),
        )),
    );
  }

  const readiness = $derived(
    store.selectedServers.length > 1 || store.unresolvedServers.length
      ? store.selectionValidation
      : panelReadiness(store.connections, store.latencyEnabled),
  );
  const READINESS_LABEL = {
    verified: "Ready",
    checking: "Checking paths",
    failed: "Path failed",
    stale: "Recheck needed",
  } as const;
</script>

<div class="setup-grid">
  <h2 class="caps tier-label">Test</h2>
  <section class="surface-inset panel wide primary">
    <div class="section-heading">
      <h3 class="caps">Connection paths</h3>
      <span
        class="badge"
        data-readiness={readiness}
        data-tone={readiness === "verified"
          ? "ok"
          : readiness === "failed"
            ? "err"
            : "warn"}
        aria-live="polite"
        use:tooltip={readiness === "verified"
          ? "Recent successful checks are reused while the required server and path are unchanged. Expired checks are refreshed before a test starts."
          : READINESS_LABEL[readiness]}
      >
        {READINESS_LABEL[readiness]}
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
              step="500"
              disabled={store.preparing}
              value={store.config.duration[key]}
              onchange={(event) => setDuration(key, event)}
            />
          </label>
        {/each}
      </div>
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
            use:tooltip={JARGON.unitBits}
            onclick={() => (store.unitKind = "bits")}>Bits</button
          >
          <button
            type="button"
            aria-pressed={store.unitKind === "bytes"}
            use:tooltip={JARGON.unitBytes}
            onclick={() => (store.unitKind = "bytes")}>Bytes</button
          >
        </div>
      </div>
      <div class="field">
        <span>Prefix</span>
        <div class="segmented" role="group" aria-label="Prefix scale">
          <button
            type="button"
            aria-pressed={store.unitBase === "base10"}
            use:tooltip={JARGON.unitDecimal}
            onclick={() => (store.unitBase = "base10")}>Decimal</button
          >
          <button
            type="button"
            aria-pressed={store.unitBase === "base2"}
            use:tooltip={JARGON.unitBinary}
            onclick={() => (store.unitBase = "base2")}>Binary</button
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
        (store.resultHistoryPreference = enabled ? "enabled" : "disabled")}
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
      bind:checked={store.showWireEstimates}
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
      checked={store.config.adaptive.enabled}
      onToggle={setAdaptiveEnabled}
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
          bind:value={store.config[key]}
          disabled={running || store.preparing}
        >
          {#each Object.entries(PING_CADENCE) as [value, name] (value)}
            <option {value}>{name}</option>
          {/each}
        </select>
      </label>
    {/each}
    <Switch
      bind:checked={store.config.skipLoadedLatencyWhenStageOff}
      disabled={running || store.preparing}
      label="Skip loaded latency when latency is off"
    />
  </section>
  <section class="surface-inset panel">
    <h3 class="caps">Datagram throughput</h3>
    {#if store.config.experimentalDatagramThroughput || datagramSelected}
      <!-- Above the toggle: this panel ends a long scroll, and a note past the
           control that summoned it is a note nobody reads. Announced as a
           status rather than an alert — nothing has gone wrong — and its point
           is carried by the leading sentence, not only by the warn colour. -->
      <p class="notice" data-tone="warn" role="status">
        <strong>Measures application datagram delivery.</strong> Datagrams are not
        retransmitted. Missing deliveries can come from network or endpoint queues;
        they do not identify physical packet loss. Expect a lower received rate than
        stream transfers, especially for browser uploads.
      </p>
    {/if}
    <Switch
      bind:checked={store.config.experimentalDatagramThroughput}
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
      onToggle={setForcedStreams}
      disabled={running || store.preparing}
      label="Force exact stream count"
      tooltip="Automatic chooses concurrency for each protocol. Forced uses the exact count per server and direction within shared connection limits."
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
            store.config.transferStreams.count,
            normalizeStreamCount,
            (count) => (store.config.transferStreams.count = count),
          )}
      />
    </label>
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
  description="Restore test, display, and history-saving settings to their defaults? Your theme, panel layout, and saved results will be kept."
  cancelLabel="Keep settings"
  confirmLabel="Reset settings"
  onCancel={() => (resetConfirmOpen = false)}
  onConfirm={resetSettings}
/>

<style>
  .setup-grid {
    display: grid;
    /* Connection choice cards inherit this exact breakpoint so the Settings
       surface reflows as one system when its dock is manually widened. */
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
