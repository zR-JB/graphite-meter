<script lang="ts">
  // Settings test setup: stage selection, timing, endpoint, units, and
  // compensation controls bound directly into the app store.
  import { store, DURATION_PRESETS } from "../../state/store.svelte";
  import type {
    RunnerConfig,
    ConnectionProfile,
    CompensationTransportSetting,
  } from "../../runner/contract";
  import { applyConnectionProfile } from "../../compensation";
  import { tooltip, JARGON } from "../../actions/tooltip";
  import Switch from "../Switch.svelte";

  interface Props {
    /** Mirrors store.isRunning — locks mid-run-unsafe inputs. */
    running?: boolean;
  }
  let { running = false }: Props = $props();

  type PresetKey = "short" | "medium" | "long" | "custom";
  const PRESET_KEYS: PresetKey[] = ["short", "medium", "long", "custom"];
  type Durations = RunnerConfig["duration"];
  const DURATION_KEYS = [
    "warmupMs",
    "latencyMs",
    "downloadMs",
    "uploadMs",
    "bidirectionalMs",
  ] as const;
  const DUR_FIELDS = [
    { key: "warmupMs", label: "Warmup", inputLabel: "Warmup ms" },
    { key: "latencyMs", label: "Latency", inputLabel: "Latency ms" },
    { key: "downloadMs", label: "Download", inputLabel: "Download ms" },
    { key: "uploadMs", label: "Upload", inputLabel: "Upload ms" },
  ] as const;
  const BIDIRECTIONAL_FIELD = {
    key: "bidirectionalMs",
    label: "Bi-dir",
    inputLabel: "Bidirectional ms",
  } as const;

  function durationsEqual(a: Durations, b: Durations): boolean {
    return DURATION_KEYS.every((key) => a[key] === b[key]);
  }
  function presetFromDurations(): PresetKey {
    for (const k of ["short", "medium", "long"] as const) {
      if (durationsEqual(store.config.duration, DURATION_PRESETS[k])) return k;
    }
    return "custom";
  }
  let durationMode = $state<PresetKey>(presetFromDurations());

  function applyPreset(k: PresetKey) {
    durationMode = k;
    if (k !== "custom") store.config.duration = { ...DURATION_PRESETS[k] };
  }
  const presetCells = $derived.by<{ label: string; value: string }[]>(() => {
    if (durationMode === "custom") return [];
    const d = DURATION_PRESETS[durationMode];
    const s = (ms: number) => `${+(ms / 1000).toFixed(1)}s`;
    const fields = store.config.stages.bidirectional
      ? [...DUR_FIELDS, BIDIRECTIONAL_FIELD]
      : DUR_FIELDS;
    return fields.map((f) => ({
      label: f.label,
      value: s(d[f.key]),
    }));
  });

  /* ---------- Visualization throughput max ----------
   * Stored as bytesPerSec (or "auto"); edited in the active display unit. */
  const vizAuto = $derived(
    store.config.visualization.throughputMaxBytesPerSec === "auto",
  );
  // Display value = bytesPerSec → active unit (matches the gauge/chart label).
  const vizDisplay = $derived(
    vizAuto
      ? 0
      : store.toUnit(
          store.config.visualization.throughputMaxBytesPerSec as number,
        ),
  );
  // Inverse of vizDisplay: a value typed in the active unit → raw bytes/s. Uses
  // store.fromUnit so it tracks the same dynamic prefix the field displays in
  // (e.g. Gbit/s for a multi-gigabit ceiling), keeping the round-trip lossless.
  function toBytesPerSec(displayValue: number): number {
    return Math.max(1, Math.round(store.fromUnit(displayValue)));
  }
  function setVizAuto(auto: boolean) {
    if (auto) {
      store.config.visualization.throughputMaxBytesPerSec = "auto";
    } else {
      // Seed the manual ceiling from the current effective scale (raw bytes/s;
      // has a sane idle default) — never round-trip through the display unit,
      // whose prefix index is degenerate before any throughput data exists.
      store.config.visualization.throughputMaxBytesPerSec = Math.max(
        1,
        Math.round(store.displayScaleBytesPerSec),
      );
    }
  }
  function onVizInput(e: Event) {
    const n = Number((e.currentTarget as HTMLInputElement).value);
    if (!Number.isFinite(n) || n <= 0) return;
    store.config.visualization.throughputMaxBytesPerSec = toBytesPerSec(n);
  }

  /* Physical-path presets seed facts the browser cannot detect. */
  const PROFILE_OPTIONS: { value: ConnectionProfile; label: string }[] = [
    { value: "lan", label: "Local Ethernet (LAN)" },
    { value: "loopback", label: "Loopback / same host" },
    { value: "tunnel", label: "VPN / UDP tunnel" },
    { value: "custom", label: "Custom" },
  ];
  const TRANSPORT_OPTIONS: {
    value: CompensationTransportSetting;
    label: string;
  }[] = [
    { value: "auto", label: "Automatic (browser detected)" },
    { value: "http1-clear", label: "HTTP/1.1 (cleartext)" },
    { value: "https-tls", label: "HTTPS (TLS)" },
    { value: "http2", label: "HTTP/2 (TLS)" },
    { value: "http3-quic", label: "HTTP/3 (QUIC)" },
  ];
  function reseedProfile() {
    const preset = applyConnectionProfile(store.config.compensation.profile);
    const ipVersion = store.config.compensation.params.ipVersion;
    Object.assign(store.config.compensation.params, preset.params);
    store.config.compensation.params.ipVersion = ipVersion;
  }

  /* ---------- Compensation numeric params (Advanced) ---------- */
  const COMP_NUMS = [
    { key: "mtuBytes", label: "MTU bytes", min: 576, max: 65536, step: 1 },
    {
      key: "tcpOptionsMinBytes",
      label: "TCP options min B",
      min: 0,
      max: 40,
      step: 4,
    },
    {
      key: "tcpOptionsMaxBytes",
      label: "TCP options max B",
      min: 0,
      max: 40,
      step: 4,
    },
    {
      key: "encapsulationBytes",
      label: "Tunnel overhead B",
      min: 0,
      max: 256,
      step: 1,
    },
    {
      key: "quicConnIdMinBytes",
      label: "QUIC CID min B",
      min: 0,
      max: 20,
      step: 1,
    },
    {
      key: "quicConnIdMaxBytes",
      label: "QUIC CID max B",
      min: 0,
      max: 20,
      step: 1,
    },
  ] as const;
</script>

<div class="setup-grid">
  <h2 class="tier-label">Test</h2>

  <section class="panel wide primary">
    <h3>Test plan</h3>
    <label>
      <span>Network target</span>
      <select disabled={running} bind:value={store.config.endpoint.protocol}>
        <option value="current">Current origin</option>
        <option
          value="http1"
          disabled={store.infra?.availableTargets?.http1 === false}
          >HTTP/1.1</option
        >
        <option
          value="http2"
          disabled={store.infra?.availableTargets?.http2 === false}
          >HTTP/2</option
        >
        <option
          value="http3"
          disabled={store.infra?.availableTargets?.http3 === false}
          >HTTP/3</option
        >
      </select>
    </label>
    <p class="hint">Secure pages cannot select the clear HTTP/1.1 target.</p>
    <div class="seg" role="group" aria-label="Duration preset">
      {#each PRESET_KEYS as k (k)}
        <button
          type="button"
          aria-pressed={durationMode === k}
          class:active={durationMode === k}
          disabled={running}
          onclick={() => applyPreset(k)}
        >
          {k}
        </button>
      {/each}
    </div>
    <Switch
      disabled={running}
      bind:checked={store.config.stages.bidirectional}
      label="Include concurrent download + upload"
    />
    {#if durationMode === "custom"}
      <div class="duration-fields">
        {#each DUR_FIELDS as f (f.key)}
          <label>
            <span>{f.inputLabel}</span>
            <input
              type="number"
              min="0"
              step="500"
              disabled={running}
              bind:value={store.config.duration[f.key]}
            />
          </label>
        {/each}
        {#if store.config.stages.bidirectional}
          <label>
            <span>{BIDIRECTIONAL_FIELD.inputLabel}</span>
            <input
              type="number"
              min="0"
              step="500"
              disabled={running}
              bind:value={store.config.duration.bidirectionalMs}
            />
          </label>
        {/if}
      </div>
      <p class="hint">Custom stage durations are in milliseconds.</p>
    {:else}
      <div class="dur-summary">
        {#each presetCells as c (c.label)}
          <div class="dur-cell">
            <span>{c.label}</span>
            <strong>{c.value}</strong>
          </div>
        {/each}
      </div>
    {/if}
    <p class="hint">
      Bidirectional adds a real-world two-way load phase. Named presets apply
      the matching preset duration automatically.
    </p>
  </section>

  <h2 class="tier-label">Results</h2>

  <!-- Display Units -->
  <section class="panel">
    <h3>Display Units</h3>
    <div class="two">
      <div class="field">
        <span>Rate unit</span>
        <div class="seg" role="group" aria-label="Rate unit">
          <button
            type="button"
            class:active={store.unitKind === "bits"}
            aria-pressed={store.unitKind === "bits"}
            use:tooltip={"Bits per second — Mbit/s, Gbit/s"}
            onclick={() => (store.unitKind = "bits")}>Bits</button
          >
          <button
            type="button"
            class:active={store.unitKind === "bytes"}
            aria-pressed={store.unitKind === "bytes"}
            use:tooltip={"Bytes per second — MB/s, GB/s"}
            onclick={() => (store.unitKind = "bytes")}>Bytes</button
          >
        </div>
      </div>
      <div class="field">
        <span>Prefix scale</span>
        <div class="seg" role="group" aria-label="Prefix scale">
          <button
            type="button"
            class:active={store.unitBase === "base10"}
            aria-pressed={store.unitBase === "base10"}
            use:tooltip={"SI prefixes, 1000 per step — Mbit/s, Gbit/s"}
            onclick={() => (store.unitBase = "base10")}>Decimal</button
          >
          <button
            type="button"
            class:active={store.unitBase === "base2"}
            aria-pressed={store.unitBase === "base2"}
            use:tooltip={"IEC prefixes, 1024 per step — Mibit/s, Gibit/s"}
            onclick={() => (store.unitBase = "base2")}>Binary</button
          >
        </div>
      </div>
    </div>
    <p class="hint">
      Applies to every rate shown; the measurement itself is unchanged.
    </p>
  </section>

  <section class="panel">
    <h3>Gauge Scale</h3>
    <Switch
      checked={vizAuto}
      onToggle={setVizAuto}
      label="Scale throughput automatically"
    />
    {#if !vizAuto}
      <label>
        <span>Throughput max {store.unitLabel}</span>
        <input
          type="number"
          min="1"
          step="1"
          value={Number(vizDisplay.toFixed(2))}
          oninput={onVizInput}
        />
      </label>
    {/if}
    <p class="hint">
      Sets the gauge and chart ceiling; automatic mode follows the measured
      peak.
    </p>
  </section>

  <!-- Wire-Rate Estimates (wide) -->
  <section class="panel wide">
    <h3>Wire-Rate Estimates</h3>
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
              disabled={running}
              bind:value={store.config.compensation.profile}
              onchange={reseedProfile}
            >
              {#each PROFILE_OPTIONS as o (o.value)}
                <option value={o.value}>{o.label}</option>
              {/each}
            </select>
          </label>
          <label>
            <span use:tooltip={JARGON.compTransport}
              >Transport &amp; security</span
            >
            <select
              disabled={running}
              bind:value={store.config.compensation.transport}
            >
              {#each TRANSPORT_OPTIONS as o (o.value)}
                <option value={o.value}>{o.label}</option>
              {/each}
            </select>
          </label>
        </div>
        <p class="hint">
          Detected first hop: {store.infra?.firstHopProtocol ??
            "scheme fallback"}. Client address: {store.infra
            ? `IPv${store.infra.clientIpVersion} via ${store.infra.clientIpSource === "forwarded" ? "trusted proxy" : "socket peer"}`
            : "not detected"}. Proxy translation or overlays can make the
          address family differ from the physical path.
        </p>
        <details class="advanced">
          <summary>Advanced — raw byte accounting</summary>
          <p class="hint">
            The browser cannot inspect MTU, TCP options, VLANs, or tunnel
            headers. These bounds define the displayed estimate range.
          </p>
          <div class="two">
            <label>
              <span>IP version</span>
              <select
                disabled={running}
                bind:value={store.config.compensation.params.ipVersion}
              >
                <option value="auto">Automatic (detected)</option>
                <option value={4}>IPv4 override</option>
                <option value={6}>IPv6 override</option>
              </select>
            </label>
            {#each COMP_NUMS as n (n.key)}
              <label>
                <span>{n.label}</span>
                <input
                  type="number"
                  min={n.min}
                  max={n.max}
                  step={n.step}
                  disabled={running}
                  bind:value={store.config.compensation.params[n.key]}
                />
              </label>
            {/each}
            <div class="spanned">
              <Switch
                disabled={running}
                bind:checked={store.config.compensation.params.vlanTagged}
                label="VLAN tagged (+4B/frame)"
              />
            </div>
          </div>
        </details>
      </div>
    </details>
  </section>

  <h2 class="tier-label">Advanced</h2>

  <!-- Early Finish -->
  <section class="panel">
    <h3>Early Finish</h3>
    <Switch
      disabled={running}
      bind:checked={store.config.adaptive.enabled}
      label="Enable adaptive early phase completion"
    />
    {#if store.config.adaptive.enabled}
      <div class="two">
        <label>
          <span>Min coverage</span>
          <input
            type="number"
            min="0.25"
            max="1"
            step="0.01"
            disabled={running}
            bind:value={store.config.adaptive.minCoverageRatio}
          />
        </label>
        <label>
          <span use:tooltip={JARGON.stability}>Stability</span>
          <input
            type="number"
            min="0.5"
            max="0.99"
            step="0.01"
            disabled={running}
            bind:value={store.config.adaptive.stabilityThreshold}
          />
        </label>
        <label>
          <span>Glide (ms)</span>
          <input
            type="number"
            min="300"
            max="1500"
            step="50"
            disabled={running}
            bind:value={store.config.adaptive.glideMs}
          />
        </label>
      </div>
    {/if}
    <p class="hint">
      Finishes early once the reading stabilizes, instead of running full
      duration.
    </p>
  </section>

  <!-- Connections & Timing -->
  <section class="panel">
    <h3>Connections &amp; Timing</h3>
    <label>
      <span>Ping velocity</span>
      <select disabled={running} bind:value={store.config.pingConcurrency}>
        <option value="instant">Instant</option>
        <option value="medium">Medium</option>
        <option value="slow">Slow</option>
      </select>
    </label>
    <label>
      <span>Max parallel streams</span>
      <input
        type="number"
        min="1"
        max="6"
        step="1"
        disabled={running}
        bind:value={store.config.parallelStreams}
      />
    </label>
    <p class="hint">
      Lanes are chosen automatically per phase; this only caps the maximum.
    </p>
    <Switch
      disabled={running}
      bind:checked={store.config.skipLoadedLatencyWhenStageOff}
      label="Skip loaded-latency when the latency stage is off"
    />
    <p class="hint">
      Also skips under-load pings when the latency stage itself is off.
    </p>
  </section>

  <!-- Download Engine -->
  <section class="panel">
    <h3>Download Engine</h3>
    <Switch
      disabled={running}
      bind:checked={store.config.experimentalChunkedDownload}
      label="Chunked download (experimental)"
    />
    <p class="hint">
      Uses adaptively-sized chunks instead of one long stream per lane
      (experimental).
    </p>
  </section>
</div>

<style>
  .setup-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr));
    gap: 12px;
    container-type: inline-size;
  }
  .panel {
    display: grid;
    align-content: start;
    gap: 12px;
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background:
      linear-gradient(180deg, var(--surface-2), transparent),
      var(--surface-inset);
    padding: var(--space-3);
    box-shadow: var(--elev-recess);
  }
  .panel.wide {
    grid-column: 1 / -1;
  }

  .panel.primary {
    border-color: color-mix(in srgb, var(--brand) 24%, var(--border));
  }

  /* Tier headings keep one scrollable surface while making frequency and
     impact clear: test setup, result presentation, then advanced tuning. */
  .tier-label {
    grid-column: 1 / -1;
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

  label,
  .field {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  label span,
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
    border-radius: var(--radius-sm);
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
  input:disabled,
  select:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .two {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
  }

  .duration-fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 10px;
  }

  /* Segmented control */
  .seg {
    display: flex;
    gap: 3px;
    padding: 3px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-inset);
  }
  .seg button {
    flex: 1;
    min-height: 30px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-soft);
    font-size: 11px;
    font-weight: 700;
    text-transform: capitalize;
    cursor: pointer;
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .seg button:hover:not(:disabled) {
    color: var(--text);
  }
  .seg button.active {
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  .seg button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* Bare span used to make a single grid child span both .two columns. */
  .spanned {
    grid-column: 1 / -1;
  }

  .hint {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.55;
  }

  /* Read-only per-stage durations for a named preset. */
  .dur-summary {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(76px, 1fr));
    gap: 6px;
  }
  .dur-cell {
    display: grid;
    gap: 2px;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    min-width: 0;
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
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  /* Advanced disclosure — keeps the raw byte knobs (and, at the top level,
     the whole compensation model) out of the way until needed. */
  .advanced {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    padding: 0;
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
  .advanced[open] summary {
    border-bottom: 1px solid var(--border);
  }
  .advanced > :not(summary) {
    margin: 10px;
  }

  /* Top-level compensation disclosure has no chrome of its own (the section
     it sits in already provides that) — just the summary's own bottom rule
     when open, so it doesn't visually double-box around the nested "raw byte
     accounting" disclosure it now contains. */
  .advanced.top-level {
    border: 0;
    background: transparent;
    box-shadow: none;
    padding: 0;
  }
  .advanced.top-level > :not(summary) {
    margin: 12px 0 0;
  }
  .disclosure-body {
    display: grid;
    gap: 12px;
  }

  @container (max-width: 360px) {
    .two {
      grid-template-columns: 1fr;
    }
    .spanned {
      grid-column: auto;
    }
  }
</style>
