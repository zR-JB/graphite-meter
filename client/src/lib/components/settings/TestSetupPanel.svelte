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
  import { panelReadiness } from "../../runner/connectionModel";
  import { JARGON, tooltip } from "../../actions/tooltip";
  import Switch from "../Switch.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";

  interface Props {
    running?: boolean;
    onOpenHistory?: (invoker: HTMLElement) => void;
  }
  let { running = false, onOpenHistory }: Props = $props();

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
  const throughputTargets = $derived([
    { value: "auto", label: "Automatic" },
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
  ]);
  const latencyTargets = $derived([
    { value: "auto", label: "Automatic" },
    ...Object.values(store.transportDiscovery?.latency ?? {}).flatMap((entry) =>
      entry.targets.map((target) => targetOption(target)),
    ),
  ]);

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
  function presetFromDuration(): Preset {
    for (const key of ["short", "medium", "long"] as const)
      if (sameDuration(store.config.duration, DURATION_PRESETS[key]))
        return key;
    return "custom";
  }
  let durationMode = $state<Preset>(presetFromDuration());
  function setPreset(preset: Preset) {
    durationMode = preset;
    if (preset !== "custom") {
      controller.configureRun({ duration: { ...DURATION_PRESETS[preset] } });
    }
  }
  function setDuration(key: DurationKey, event: Event) {
    const value = Number((event.currentTarget as HTMLInputElement).value);
    if (!Number.isFinite(value) || value < 0) return;
    controller.configureRun({
      duration: { ...store.config.duration, [key]: value },
    });
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
      value: `${+(DURATION_PRESETS[preset][key] / 1000).toFixed(1)}s`,
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
    const value = Number((event.currentTarget as HTMLInputElement).value);
    if (Number.isFinite(value) && value > 0)
      store.config.visualization.throughputMaxBytesPerSec = Math.max(
        1,
        Math.round(store.fromUnit(value)),
      );
  }

  const readiness = $derived(
    panelReadiness(store.connections, store.latencyEnabled),
  );
  const READINESS_LABEL = {
    verified: "Ready",
    checking: "Checking paths",
    failed: "Path failed",
    stale: "Recheck needed",
  } as const;
</script>

<div class="setup-grid">
  <h2 class="tier-label">Test</h2>

  <section class="panel wide primary">
    <div class="section-heading">
      <h3>Connection paths</h3>
      <span class="readiness-badge" data-state={readiness} aria-live="polite">
        {READINESS_LABEL[readiness]}
      </span>
    </div>
    <p class="intro">
      Throughput carries downloads, uploads, and upload progress. Latency uses
      its own independently selected path.
    </p>
    <ConnectionPicker
      role="throughput"
      options={throughputTargets}
      locked={running}
    />
    <ConnectionPicker
      role="latency"
      options={latencyTargets}
      locked={running}
    />
  </section>

  <section class="panel">
    <h3>Duration &amp; stages</h3>
    <div class="seg" role="group" aria-label="Duration preset">
      {#each PRESETS as preset}
        <button
          type="button"
          class:active={durationMode === preset}
          aria-pressed={durationMode === preset}
          onclick={() => setPreset(preset)}>{preset}</button
        >
      {/each}
    </div>
    <Switch
      checked={store.config.stages.bidirectional}
      onToggle={setBidirectional}
      disabled={running && store.phaseStage === "bidirectional"}
      label="Include concurrent download + upload"
    />
    {#if durationMode === "custom"}
      <div class="duration-fields">
        {#each activeDurationFields as [key, label]}
          <label>
            <span>{label} ms</span>
            <input
              type="number"
              min="0"
              step="500"
              value={store.config.duration[key]}
              oninput={(event) => setDuration(key, event)}
            />
          </label>
        {/each}
      </div>
    {:else}
      <div class="dur-summary">
        {#each presetCells as cell}
          <div class="dur-cell">
            <span>{cell.label}</span>
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

  <h2 class="tier-label">Results</h2>

  <section class="panel">
    <h3>Display units</h3>
    <div class="two">
      <div class="field">
        <span>Rate</span>
        <div class="seg" role="group" aria-label="Rate unit">
          <button
            type="button"
            class:active={store.unitKind === "bits"}
            aria-pressed={store.unitKind === "bits"}
            use:tooltip={JARGON.unitBits}
            onclick={() => (store.unitKind = "bits")}>Bits</button
          >
          <button
            type="button"
            class:active={store.unitKind === "bytes"}
            aria-pressed={store.unitKind === "bytes"}
            use:tooltip={JARGON.unitBytes}
            onclick={() => (store.unitKind = "bytes")}>Bytes</button
          >
        </div>
      </div>
      <div class="field">
        <span>Prefix</span>
        <div class="seg" role="group" aria-label="Prefix scale">
          <button
            type="button"
            class:active={store.unitBase === "base10"}
            aria-pressed={store.unitBase === "base10"}
            use:tooltip={JARGON.unitDecimal}
            onclick={() => (store.unitBase = "base10")}>Decimal</button
          >
          <button
            type="button"
            class:active={store.unitBase === "base2"}
            aria-pressed={store.unitBase === "base2"}
            use:tooltip={JARGON.unitBinary}
            onclick={() => (store.unitBase = "base2")}>Binary</button
          >
        </div>
      </div>
    </div>
    <p class="hint">
      Applies to every displayed rate; measurement values are unchanged.
    </p>
  </section>

  <section class="panel wide">
    <h3>Result history</h3>
    <Switch
      checked={store.savingResults}
      onToggle={(enabled) =>
        (store.resultHistoryPreference = enabled ? "enabled" : "disabled")}
      label="Save completed results on this device"
    />
    <a
      class="history-link"
      href="#/history"
      onclick={(event) => {
        if (!onOpenHistory) return;
        event.preventDefault();
        onOpenHistory(event.currentTarget as HTMLElement);
      }}>View History</a
    >
  </section>

  <section class="panel wide">
    <h3>Wire-rate estimates</h3>
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

  <section class="panel">
    <h3>Gauge scale</h3>
    <Switch
      checked={vizAuto}
      onToggle={setVizAuto}
      label="Scale throughput automatically"
    />
    {#if !vizAuto}
      <label>
        <span>Maximum {store.unitLabel}</span>
        <input
          type="number"
          min="1"
          value={Number(vizDisplay.toFixed(2))}
          oninput={setVizMax}
        />
      </label>
    {/if}
    <p class="hint">
      The chart keeps this exact ceiling; the gauge rounds to a readable scale.
      Automatic chart scaling follows the measured peak, while the gauge starts
      at 1 Gbit/s and grows by decimal decades.
    </p>
  </section>

  <h2 class="tier-label">Advanced</h2>

  <section class="panel">
    <h3>Early finish</h3>
    <Switch
      checked={store.config.adaptive.enabled}
      onToggle={setAdaptiveEnabled}
      label="Finish stable stages early"
    />
    <p class="hint">
      Stability is estimated from the measured samples; a stage ends early when
      it is stable enough.
    </p>
  </section>

  <section class="panel">
    <h3>Latency timing</h3>
    <label>
      <span>Unloaded ping cadence</span>
      <select bind:value={store.config.pingCadence} disabled={running}>
        <option value="reply-driven">Reply-driven</option>
        <option value="fast">Fast (80 ms)</option>
        <option value="medium">Medium (250 ms)</option>
        <option value="slow">Slow (600 ms)</option>
      </select>
    </label>
    <label>
      <span>Loaded ping cadence</span>
      <select bind:value={store.config.loadedPingCadence} disabled={running}>
        <option value="reply-driven">Reply-driven</option>
        <option value="fast">Fast (80 ms)</option>
        <option value="medium">Medium (250 ms)</option>
        <option value="slow">Slow (600 ms)</option>
      </select>
    </label>
    <Switch
      bind:checked={store.config.skipLoadedLatencyWhenStageOff}
      disabled={running}
      label="Skip loaded latency when latency is off"
    />
  </section>

  <section class="panel">
    <h3>Datagram throughput</h3>
    {#if store.config.experimentalDatagramThroughput || datagramSelected}
      <!-- Above the toggle: this panel ends a long scroll, and a note past the
           control that summoned it is a note nobody reads. Announced as a
           status rather than an alert — nothing has gone wrong — and its point
           is carried by the leading sentence, not only by the warn colour. -->
      <p class="caution" role="status">
        <strong>Measures application datagram delivery.</strong> Datagrams are not
        retransmitted. Missing deliveries can come from network or endpoint queues;
        they do not identify physical packet loss. Expect a lower received rate than
        stream transfers, especially for browser uploads.
      </p>
    {/if}
    <Switch
      bind:checked={store.config.experimentalDatagramThroughput}
      disabled={running}
      label="Datagram throughput (experimental)"
    />
    <p class="hint">
      Adds the WebTransport datagram card to the connection picker.
    </p>
  </section>

  <section class="panel">
    <h3>Transfer streams</h3>
    <Switch
      checked={store.config.transferStreams.mode === "forced"}
      onToggle={setForcedStreams}
      disabled={running}
      label="Force exact stream count"
      tooltip="Automatic caps HTTP/1.1 at the configured maximum while choosing protocol-safe concurrency for HTTP/2 and HTTP/3. Forced starts the exact count per active direction."
    />
    <label>
      <span
        >{store.config.transferStreams.mode === "forced"
          ? "Streams per direction"
          : "Maximum H1 streams per direction"}</span
      >
      <input
        type="number"
        min="1"
        max="128"
        step="1"
        disabled={running}
        bind:value={store.config.transferStreams.count}
      />
    </label>
    {#if store.config.transferStreams.mode === "forced"}
      <p class="hint">
        Starts exactly {store.config.transferStreams.count} requests per active direction.
        Browser connection limits may queue HTTP/1.1 requests.
      </p>
    {:else}
      <p class="hint">
        Automatic caps HTTP/1.1 at {store.config.transferStreams.count}. HTTP/2
        and HTTP/3 choose safe multiplexed request counts automatically.
      </p>
    {/if}
  </section>
</div>

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
    container-type: inline-size;
    container-name: settings-grid;
  }
  .panel {
    display: grid;
    align-content: start;
    gap: var(--space-3);
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background:
      linear-gradient(180deg, var(--surface-2), transparent),
      var(--surface-inset);
    padding: var(--space-3);
    box-shadow: var(--elev-recess);
  }
  .wide,
  .tier-label {
    grid-column: 1 / -1;
  }
  .primary {
    border-color: color-mix(in srgb, var(--brand) 24%, var(--border));
  }
  .tier-label {
    margin: 4px 0 -4px;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.18em;
    text-transform: uppercase;
  }
  .tier-label:first-child {
    margin-top: 0;
  }
  h3 {
    margin: 0;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.13em;
    text-transform: uppercase;
  }
  .section-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .readiness-badge {
    flex: none;
    padding: 3px 7px;
    border-radius: var(--r-full);
    background: var(--warn-soft);
    color: var(--warn);
    font-size: 9px;
    font-weight: 750;
  }
  .readiness-badge[data-state="verified"] {
    background: var(--ok-soft);
    color: var(--ok);
  }
  .readiness-badge[data-state="failed"] {
    background: var(--err-soft);
    color: var(--err);
  }
  .intro {
    margin: 0;
    color: var(--text-soft);
    font-size: 10px;
    line-height: 1.5;
  }
  label,
  .field {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  label > span,
  .field > span {
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  input[type="number"],
  select {
    width: 100%;
    min-height: 36px;
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-1);
    color: var(--text);
    padding: 7px 9px;
    font-family: var(--font-mono);
    font-size: 12px;
    outline: none;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      box-shadow var(--dur-hover) var(--ease-out);
  }
  input:focus-visible,
  select:focus-visible {
    border-color: color-mix(in srgb, var(--brand) 56%, var(--border));
    box-shadow: 0 0 0 3px var(--brand-soft);
  }
  .seg {
    display: flex;
    gap: 3px;
    padding: 3px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
  }
  button {
    flex: 1;
    min-height: 30px;
    border: 0;
    border-radius: var(--r-well);
    background: transparent;
    color: var(--text-soft);
    font-family: var(--font-sans);
    font-size: 11px;
    font-weight: 700;
    cursor: pointer;
    text-transform: capitalize;
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .seg button {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  button:hover {
    color: var(--text);
  }
  button.active {
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  .two {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
    gap: 10px;
  }
  .hint {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.55;
  }
  .history-link {
    color: var(--brand-strong);
    font-size: 12px;
  }
  .caution {
    margin: 0;
    padding: 8px 10px;
    border: 1px solid color-mix(in srgb, var(--warn) 42%, transparent);
    border-radius: var(--radius-sm, 6px);
    background: var(--warn-soft);
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.55;
  }
  .caution strong {
    color: var(--warn);
  }
  .duration-fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 10px;
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
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-1);
  }
  .dur-cell span {
    overflow: hidden;
    color: var(--text-soft);
    font-size: 9px;
    font-weight: 800;
    letter-spacing: 0.06em;
    text-overflow: ellipsis;
    text-transform: uppercase;
    white-space: nowrap;
  }
  .dur-cell strong {
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }
  @container (max-width: 360px) {
    .two {
      grid-template-columns: 1fr;
      gap: var(--space-1);
    }
  }
</style>
