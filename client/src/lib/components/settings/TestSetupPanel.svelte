<script lang="ts">
  import { store, DURATION_PRESETS } from "../../state/store.svelte";
  import type {
    ConnectionProfile,
    CompensationTransportSetting,
    RunnerConfig,
  } from "../../runner/contract";
  import { applyConnectionProfile } from "../../compensation";
  import { applyStageChange } from "../../runner/wire.svelte";
  import { tooltip, JARGON } from "../../actions/tooltip";
  import Switch from "../Switch.svelte";
  import ConnectionPicker from "./ConnectionPicker.svelte";

  interface Props {
    running?: boolean;
  }
  let { running = false }: Props = $props();

  const THROUGHPUT_TARGETS = [
    { value: "current", label: "Same as this page" },
    { value: "http1-clear", label: "HTTP/1.1 clear" },
    { value: "http1-tls", label: "HTTP/1.1 TLS" },
    { value: "http2", label: "HTTP/2" },
    { value: "http3", label: "HTTP/3" },
  ] as const;
  const LATENCY_TARGETS = [
    { value: "auto", label: "Match page security" },
    { value: "ws-http1-clear", label: "WebSocket clear" },
    { value: "ws-http1-tls", label: "WebSocket TLS" },
  ] as const;

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
  const durationPreset = $derived.by<Preset>(() => {
    for (const key of ["short", "medium", "long"] as const)
      if (sameDuration(store.config.duration, DURATION_PRESETS[key]))
        return key;
    return "custom";
  });
  function setPreset(preset: Preset) {
    if (preset !== "custom")
      store.config.duration = { ...DURATION_PRESETS[preset] };
  }
  function setBidirectional(enabled: boolean) {
    store.config.stages.bidirectional = enabled;
    applyStageChange();
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
  }[] = [
    { value: "auto", label: "Automatic" },
    { value: "http1-clear", label: "HTTP/1.1 clear" },
    { value: "https-tls", label: "HTTP/1.1 TLS" },
    { value: "http2", label: "HTTP/2" },
    { value: "http3-quic", label: "HTTP/3 QUIC" },
  ];
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

<div class="settings">
  <section class="panel primary">
    <h3>Connections</h3>
    <ConnectionPicker
      role="throughput"
      options={THROUGHPUT_TARGETS}
      {running}
    />
    <ConnectionPicker role="latency" options={LATENCY_TARGETS} {running} />
    <p class="hint">Upload progress follows the throughput path.</p>
  </section>

  <section class="panel">
    <h3>Run setup</h3>
    <div class="seg" role="group" aria-label="Duration preset">
      {#each PRESETS as preset}
        <button
          type="button"
          class:active={durationPreset === preset}
          aria-pressed={durationPreset === preset}
          onclick={() => setPreset(preset)}>{preset}</button
        >
      {/each}
    </div>
    <Switch
      checked={store.config.stages.bidirectional}
      onToggle={setBidirectional}
      label="Include concurrent download and upload"
    />
    {#if running}
      <p class="hint">
        Stage toggles can update the unstarted schedule. Other edits are saved
        for the next run.
      </p>
    {/if}
  </section>

  <section class="panel">
    <h3>Stream policy</h3>
    <Switch
      checked={store.config.transferStreams.mode === "forced"}
      onToggle={(forced) =>
        (store.config.transferStreams.mode = forced ? "forced" : "auto")}
      label="Force stream count"
    />
    {#if store.config.transferStreams.mode === "forced"}
      <label>
        <span>Streams per direction</span>
        <input
          type="number"
          min="1"
          max="128"
          bind:value={store.config.transferStreams.count}
        />
      </label>
    {:else}
      <p class="hint">
        Automatic uses the verified protocol and browser connection budget.
      </p>
    {/if}
  </section>

  <section class="panel readiness" aria-live="polite">
    <h3>Start readiness</h3>
    <strong class:ready
      >{ready ? "Ready" : "Waiting for connection checks"}</strong
    >
    <span>Throughput: {store.connections.throughput.validation}</span>
    {#if store.latencyEnabled}<span
        >Latency: {store.connections.latency.validation}</span
      >{/if}
  </section>

  <section class="panel">
    <h3>Display</h3>
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
    <Switch
      checked={vizAuto}
      onToggle={setVizAuto}
      label="Automatic chart scale"
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
    <Switch
      bind:checked={store.showWireEstimates}
      label="Show wire-rate estimates"
      tooltip={JARGON.wireRate}
    />
  </section>

  <details class="advanced">
    <summary>Advanced run timing</summary>
    <div class="body">
      <div class="fields">
        {#each DURATION_FIELDS as [key, label]}
          <label>
            <span>{label} ms</span>
            <input
              type="number"
              min="0"
              step="500"
              bind:value={store.config.duration[key]}
            />
          </label>
        {/each}
      </div>
      <label>
        <span>Unloaded ping cadence</span>
        <select bind:value={store.config.pingCadence}>
          <option value="instant">80 ms</option><option value="medium"
            >250 ms</option
          ><option value="slow">600 ms</option>
        </select>
      </label>
      <label>
        <span>Loaded ping cadence</span>
        <select bind:value={store.config.loadedPingCadence}>
          <option value="instant">80 ms</option><option value="medium"
            >250 ms</option
          ><option value="slow">600 ms</option>
        </select>
      </label>
      <Switch
        bind:checked={store.config.skipLoadedLatencyWhenStageOff}
        label="Skip loaded latency when latency is off"
      />
      <Switch
        bind:checked={store.config.experimentalChunkedDownload}
        label="Chunked download (experimental)"
      />
    </div>
  </details>

  <details class="advanced">
    <summary>Adaptive completion</summary>
    <div class="body">
      <Switch
        bind:checked={store.config.adaptive.enabled}
        label="Finish stable stages early"
      />
      <div class="fields">
        <label
          ><span>Minimum coverage</span><input
            type="number"
            min="0.25"
            max="1"
            step="0.01"
            bind:value={store.config.adaptive.minCoverageRatio}
          /></label
        >
        <label
          ><span>Stability threshold</span><input
            type="number"
            min="0.5"
            max="0.99"
            step="0.01"
            bind:value={store.config.adaptive.stabilityThreshold}
          /></label
        >
        <label
          ><span>Glide ms</span><input
            type="number"
            min="300"
            max="1500"
            step="50"
            bind:value={store.config.adaptive.glideMs}
          /></label
        >
      </div>
    </div>
  </details>

  <details class="advanced">
    <summary>Wire-rate assumptions</summary>
    <div class="body">
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
      <label>
        <span>IP version</span>
        <select bind:value={store.config.compensation.params.ipVersion}>
          <option value="auto">Automatic</option><option value={4}>IPv4</option
          ><option value={6}>IPv6</option>
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
        label="VLAN tagged"
      />
    </div>
  </details>
</div>

<style>
  .settings {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr));
    gap: 12px;
  }
  .panel,
  .advanced {
    display: grid;
    align-content: start;
    gap: 12px;
    min-width: 0;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
  }
  .primary,
  .advanced {
    grid-column: 1 / -1;
  }
  h3 {
    margin: 0;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.13em;
    text-transform: uppercase;
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
    text-transform: uppercase;
  }
  input,
  select {
    width: 100%;
    min-height: 36px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--text);
    padding: 7px 9px;
    font-family: var(--font-mono);
  }
  .seg {
    display: flex;
    gap: 3px;
    padding: 3px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-inset);
  }
  button {
    flex: 1;
    min-height: 30px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-soft);
    cursor: pointer;
    text-transform: capitalize;
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
  .hint,
  .readiness span {
    margin: 0;
    color: var(--text-soft);
    font-size: 10px;
    line-height: 1.45;
  }
  .readiness strong {
    color: var(--warn);
  }
  .readiness strong.ready {
    color: var(--ok);
  }
  summary {
    color: var(--text);
    font-size: 12px;
    font-weight: 800;
    cursor: pointer;
  }
  .body {
    display: grid;
    gap: 12px;
    margin-top: 12px;
  }
</style>
