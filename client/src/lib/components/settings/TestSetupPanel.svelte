<script lang="ts">
  import { store, DURATION_PRESETS } from "../../state/store.svelte";
  import type {
    ConnectionProfile,
    CompensationTransportSetting,
    RunnerConfig,
  } from "../../runner/contract";
  import { applyConnectionProfile } from "../../compensation";
  import { applyLiveRunConfig } from "../../runner/wire.svelte";
  import { describeTarget } from "../../runner/real/targetPresentation";
  import { compensationTransportLabel } from "../../runner/protocol";
  import { tooltip, JARGON } from "../../actions/tooltip";
  import Switch from "../Switch.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";

  interface Props {
    running?: boolean;
  }
  let { running = false }: Props = $props();

  const throughputTargets = $derived([
    { value: "auto", label: "Automatic" },
    ...Object.values(store.transportDiscovery?.throughput ?? {}).flatMap(
      (entry) =>
        entry.target
          ? [
              {
                value: entry.target.origin,
                label: describeTarget(
                  store.transportDiscovery!,
                  entry.target,
                  store.connections.throughput.target?.origin ===
                    entry.target.origin
                    ? store.connections.throughput.observedProtocol
                    : undefined,
                ).label,
              },
            ]
          : [],
    ),
  ]);
  const latencyTargets = $derived([
    { value: "auto", label: "Automatic" },
    ...Object.values(store.transportDiscovery?.latency ?? {}).flatMap(
      (entry) =>
        entry.target
          ? [
              {
                value: entry.target.origin,
                label: describeTarget(store.transportDiscovery!, entry.target)
                  .label,
              },
            ]
          : [],
    ),
  ]);

  type Preset = "short" | "medium" | "long" | "custom";
  const PRESETS: Preset[] = ["short", "medium", "long", "custom"];
  const DURATION_KEYS = [
    "warmupMs",
    "latencyMs",
    "downloadMs",
    "uploadMs",
    "bidirectionalMs",
  ] as const;
  const DURATION_FIELDS = [
    ["warmupMs", "Warmup"],
    ["latencyMs", "Latency"],
    ["downloadMs", "Download"],
    ["uploadMs", "Upload"],
    ["bidirectionalMs", "Bidirectional"],
  ] as const;
  function sameDuration(
    a: RunnerConfig["duration"],
    b: RunnerConfig["duration"],
  ) {
    return DURATION_KEYS.every((key) => a[key] === b[key]);
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
      store.config.duration = { ...DURATION_PRESETS[preset] };
      applyLiveRunConfig();
    }
  }
  function setDuration(key: (typeof DURATION_KEYS)[number], event: Event) {
    const value = Number((event.currentTarget as HTMLInputElement).value);
    if (!Number.isFinite(value) || value < 0) return;
    store.config.duration[key] = value;
    applyLiveRunConfig();
  }
  function setBidirectional(enabled: boolean) {
    store.config.stages.bidirectional = enabled;
    applyLiveRunConfig();
  }
  function setAdaptiveEnabled(enabled: boolean) {
    store.config.adaptive.enabled = enabled;
    applyLiveRunConfig();
  }
  function setAdaptiveNumber(
    key: "minCoverageRatio" | "stabilityThreshold" | "glideMs",
    event: Event,
  ) {
    const value = Number((event.currentTarget as HTMLInputElement).value);
    if (!Number.isFinite(value)) return;
    store.config.adaptive[key] = value;
    applyLiveRunConfig();
  }
  const presetCells = $derived.by(() => {
    const preset = durationMode;
    if (preset === "custom") return [];
    const fields = store.config.stages.bidirectional
      ? DURATION_FIELDS
      : DURATION_FIELDS.filter(([key]) => key !== "bidirectionalMs");
    return fields.map(([key, label]) => ({
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
      : Math.max(1, Math.round(store.displayScaleBytesPerSec));
  }
  function setVizMax(event: Event) {
    const value = Number((event.currentTarget as HTMLInputElement).value);
    if (Number.isFinite(value) && value > 0)
      store.config.visualization.throughputMaxBytesPerSec = Math.max(
        1,
        Math.round(store.fromUnit(value)),
      );
  }

  const PROFILES: { value: ConnectionProfile; label: string }[] = [
    { value: "lan", label: "Local Ethernet" },
    { value: "loopback", label: "Loopback" },
    { value: "tunnel", label: "VPN / tunnel" },
    { value: "custom", label: "Custom" },
  ];
  const COMPENSATION_TRANSPORTS: {
    value: CompensationTransportSetting;
    label: string;
  }[] = (
    ["auto", "http1-clear", "https-tls", "http2", "http3-quic"] as const
  ).map((value) => ({
    value,
    label: compensationTransportLabel(value),
  }));
  const COMPENSATION_NUMBERS = [
    ["mtuBytes", "MTU bytes", 576, 65536, 1],
    ["tcpOptionsMinBytes", "TCP options min", 0, 40, 4],
    ["tcpOptionsMaxBytes", "TCP options max", 0, 40, 4],
    ["encapsulationBytes", "Tunnel overhead", 0, 256, 1],
    ["quicConnIdMinBytes", "QUIC CID min", 0, 20, 1],
    ["quicConnIdMaxBytes", "QUIC CID max", 0, 20, 1],
  ] as const;
  function reseedProfile() {
    const ipVersion = store.config.compensation.params.ipVersion;
    const preset = applyConnectionProfile(store.config.compensation.profile);
    Object.assign(store.config.compensation.params, preset.params, {
      ipVersion,
    });
  }

  const ready = $derived(
    store.connections.throughput.validation === "verified" &&
      (!store.latencyEnabled ||
        store.connections.latency.validation === "verified"),
  );
</script>

<div class="setup-grid">
  <h2 class="tier-label">Test</h2>

  <section class="panel wide primary">
    <div class="section-heading">
      <h3>Connection paths</h3>
      <span class="readiness-badge" class:ready aria-live="polite">
        {ready ? "Ready" : "Checking paths"}
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
        {#each DURATION_FIELDS.filter(([key]) => store.config.stages.bidirectional || key !== "bidirectionalMs") as [key, label]}
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
            onclick={() => (store.unitKind = "bits")}>Bits</button
          >
          <button
            type="button"
            class:active={store.unitKind === "bytes"}
            aria-pressed={store.unitKind === "bytes"}
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
            onclick={() => (store.unitBase = "base10")}>Decimal</button
          >
          <button
            type="button"
            class:active={store.unitBase === "base2"}
            aria-pressed={store.unitBase === "base2"}
            onclick={() => (store.unitBase = "base2")}>Binary</button
          >
        </div>
      </div>
    </div>
    <p class="hint">
      Applies to every displayed rate; measurement values are unchanged.
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
      Sets the gauge and chart ceiling. Automatic follows the measured peak.
    </p>
  </section>

  <section class="panel wide">
    <h3>Wire-rate estimates</h3>
    <Switch
      bind:checked={store.showWireEstimates}
      label="Include wire-rate estimates in result cards"
      tooltip={JARGON.wireRate}
    />
    <p class="hint">
      Forward-direction Ethernet estimate from protocol bytes only.
    </p>
    <details class="advanced top-level">
      <summary>Customize the compensation model</summary>
      <div class="disclosure-body">
        <div class="two">
          <label>
            <span use:tooltip={JARGON.compProfile}>Connection profile</span>
            <select
              bind:value={store.config.compensation.profile}
              onchange={reseedProfile}
            >
              {#each PROFILES as option}<option value={option.value}
                  >{option.label}</option
                >{/each}
            </select>
          </label>
          <label>
            <span>Transport override</span>
            <select bind:value={store.config.compensation.transport}>
              {#each COMPENSATION_TRANSPORTS as option}<option
                  value={option.value}>{option.label}</option
                >{/each}
            </select>
          </label>
        </div>
        <details class="advanced">
          <summary>Advanced — raw byte accounting</summary>
          <div class="disclosure-body nested">
            <label>
              <span>IP version</span>
              <select bind:value={store.config.compensation.params.ipVersion}>
                <option value="auto">Automatic</option>
                <option value={4}>IPv4 override</option>
                <option value={6}>IPv6 override</option>
              </select>
            </label>
            <div class="fields">
              {#each COMPENSATION_NUMBERS as [key, label, min, max, step]}
                <label
                  ><span>{label}</span><input
                    type="number"
                    {min}
                    {max}
                    {step}
                    bind:value={store.config.compensation.params[key]}
                  /></label
                >
              {/each}
            </div>
            <Switch
              bind:checked={store.config.compensation.params.vlanTagged}
              label="VLAN tagged (+4B/frame)"
            />
          </div>
        </details>
      </div>
    </details>
  </section>

  <h2 class="tier-label">Advanced</h2>

  <section class="panel">
    <h3>Early finish</h3>
    <Switch
      checked={store.config.adaptive.enabled}
      onToggle={setAdaptiveEnabled}
      label="Finish stable stages early"
    />
    {#if store.config.adaptive.enabled}
      <div class="fields">
        <label
          ><span>Minimum coverage</span><input
            type="number"
            min="0.25"
            max="1"
            step="0.01"
            value={store.config.adaptive.minCoverageRatio}
            oninput={(event) => setAdaptiveNumber("minCoverageRatio", event)}
          /></label
        >
        <label
          ><span>Stability threshold</span><input
            type="number"
            min="0.5"
            max="0.99"
            step="0.01"
            value={store.config.adaptive.stabilityThreshold}
            oninput={(event) => setAdaptiveNumber("stabilityThreshold", event)}
          /></label
        >
        <label
          ><span>Glide ms</span><input
            type="number"
            min="300"
            max="1500"
            step="50"
            value={store.config.adaptive.glideMs}
            oninput={(event) => setAdaptiveNumber("glideMs", event)}
          /></label
        >
      </div>
    {/if}
    <p class="hint">Stops a stable stage before its full duration expires.</p>
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
    <h3>Download engine</h3>
    <Switch
      bind:checked={store.config.experimentalChunkedDownload}
      disabled={running}
      label="Chunked download (experimental)"
    />
    <p class="hint">
      Uses adaptive chunks instead of one long request per lane.
    </p>
  </section>
</div>

<style>
  .setup-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr));
    gap: var(--space-3);
    container-type: inline-size;
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
  .readiness-badge.ready {
    background: var(--ok-soft);
    color: var(--ok);
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
  button:hover {
    color: var(--text);
  }
  button.active {
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  .two,
  .fields {
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
  .duration-fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 10px;
  }
  .dur-summary {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(76px, 1fr));
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
  .advanced {
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
  }
  .advanced summary {
    cursor: pointer;
    padding: 10px;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    user-select: none;
  }
  .advanced summary:hover {
    color: var(--text);
  }
  .advanced[open] > summary {
    border-bottom: 1px solid var(--border);
  }
  .advanced.top-level {
    border: 0;
    background: transparent;
  }
  .advanced.top-level[open] > summary {
    border-bottom: 1px solid var(--border);
  }
  .disclosure-body {
    display: grid;
    gap: var(--space-3);
    margin-top: var(--space-3);
  }
  .disclosure-body.nested {
    margin: 10px;
  }
  @container (max-width: 360px) {
    .two {
      grid-template-columns: 1fr;
      gap: var(--space-1);
    }
  }
</style>
