<script lang="ts">
  /* ============================================================
   * <TestSetupPanel> — Settings › Test Setup
   * Every control two-way binds to `console.config` (or the unit
   * display prefs), so edits reflect instantly in the rail ETA,
   * gauge, chart, and unit labels. Inputs that are unsafe to
   * change mid-run are disabled while `running`.
   * ============================================================ */
  import { store, DURATION_PRESETS } from "../../state/store.svelte";
  import type {
    RunnerConfig,
    ConnectionProfile,
    CompensationTransport,
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
  const PRESET_SUMMARY_FIELDS = [
    ...DUR_FIELDS,
    { key: "bidirectionalMs", label: "Bi-dir", inputLabel: "Bi-dir ms" },
  ] as const;

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
  function onDurationEdit() {
    durationMode = "custom";
  }

  const presetCells = $derived.by<{ label: string; value: string }[]>(() => {
    if (durationMode === "custom") return [];
    const d = DURATION_PRESETS[durationMode];
    const s = (ms: number) => `${+(ms / 1000).toFixed(1)}s`;
    return PRESET_SUMMARY_FIELDS.map((f) => ({
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

  /* ---------- Connection profile + transport presets ----------
   * Picking either seeds the factor/param defaults via applyConnectionProfile;
   * the raw knobs stay editable in the Advanced disclosure below. */
  const PROFILE_OPTIONS: { value: ConnectionProfile; label: string }[] = [
    { value: "lan", label: "Local Ethernet (LAN)" },
    { value: "loopback", label: "Loopback / same host" },
    { value: "tunnel", label: "VPN tunnel (WireGuard/Tailscale)" },
    { value: "internet", label: "Internet" },
  ];
  const TRANSPORT_OPTIONS: { value: CompensationTransport; label: string }[] = [
    { value: "http1-clear", label: "HTTP/1.1 (cleartext)" },
    { value: "https-tls", label: "HTTPS (TLS)" },
    { value: "http2", label: "HTTP/2 (TLS)" },
    { value: "http3-quic", label: "HTTP/3 (QUIC)" },
  ];
  /** Re-seed factor + param defaults whenever the profile or transport changes. */
  function reseedProfile() {
    const preset = applyConnectionProfile(
      store.config.compensation.profile,
      store.config.compensation.transport,
    );
    Object.assign(store.config.compensation.factors, preset.factors);
    Object.assign(store.config.compensation.params, preset.params);
  }

  /* ---------- Compensation factor groups (de-magicked labels) ---------- */
  type FactorKey = keyof RunnerConfig["compensation"]["factors"];
  const COMP_GROUPS: {
    label: string;
    tip: string;
    toggles: { key: FactorKey; label: string }[];
  }[] = [
    {
      label: "Protocol bytes",
      tip: JARGON.compProtocol,
      toggles: [
        { key: "ethernetFraming", label: "Ethernet / IP / transport" },
        { key: "encapsulation", label: "VPN tunnel encapsulation" },
        { key: "tlsRecords", label: "TLS records" },
        { key: "applicationFraming", label: "HTTP / WS / QUIC framing" },
      ],
    },
    {
      label: "Path behavior",
      tip: JARGON.compPath,
      toggles: [
        { key: "reversePathControl", label: "ACK / control traffic" },
        { key: "lossRetransmission", label: "Loss / retransmission" },
      ],
    },
    {
      label: "Measurement model",
      tip: JARGON.compModel,
      toggles: [
        { key: "receiverBias", label: "Browser receive cost (download)" },
        { key: "steadyStateRamp", label: "Steady-state ramp" },
        { key: "browserRuntime", label: "Browser runtime tax" },
      ],
    },
  ];

  /* ---------- Compensation numeric params (Advanced) ---------- */
  const COMP_NUMS = [
    { key: "mtuBytes", label: "MTU bytes", min: 576, max: 65536, step: 1 },
    {
      key: "tcpOptionsBytes",
      label: "TCP options B",
      min: 0,
      max: 40,
      step: 4,
    },
    {
      key: "encapsulationBytes",
      label: "Encapsulation B",
      min: 0,
      max: 128,
      step: 1,
    },
    {
      key: "framePayloadBytes",
      label: "Frame payload B",
      min: 256,
      max: 65536,
      step: 256,
    },
    { key: "tlsRecordBytes", label: "TLS record B", min: 0, max: 64, step: 1 },
    { key: "aeadTagBytes", label: "AEAD tag B", min: 0, max: 255, step: 1 },
    { key: "quicConnIdBytes", label: "QUIC CID B", min: 0, max: 20, step: 1 },
    {
      key: "maxLossRatio",
      label: "Max loss ratio",
      min: 0,
      max: 1,
      step: 0.01,
    },
  ] as const;
</script>

<div class="setup-grid">
  <!-- "Run" tier: settings a user plausibly changes every session. -->
  <h2 class="tier-label">Run</h2>

  <!-- Duration -->
  <section class="panel">
    <h3>Duration</h3>
    <div class="seg" role="tablist" aria-label="Duration preset">
      {#each PRESET_KEYS as k (k)}
        <button
          role="tab"
          aria-selected={durationMode === k}
          class:active={durationMode === k}
          disabled={running}
          onclick={() => applyPreset(k)}
        >
          {k}
        </button>
      {/each}
    </div>
    {#if durationMode === "custom"}
      <div class="two">
        {#each DUR_FIELDS as f (f.key)}
          <label>
            <span>{f.inputLabel}</span>
            <input
              type="number"
              min="0"
              step="500"
              disabled={running}
              oninput={onDurationEdit}
              bind:value={store.config.duration[f.key]}
            />
          </label>
        {/each}
      </div>
      <p class="hint">Set each stage's duration in milliseconds.</p>
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
  </section>

  <!-- Bidirectional -->
  <section class="panel">
    <h3>Bidirectional</h3>
    <Switch
      disabled={running}
      bind:checked={store.config.stages.bidirectional}
      label="Bidirectional (concurrent down + up)"
    />
    <label>
      <span>Bidirectional duration (ms)</span>
      <input
        type="number"
        min="0"
        step="500"
        disabled={running || !store.config.stages.bidirectional}
        bind:value={store.config.duration.bidirectionalMs}
      />
    </label>
    <p class="hint">
      Adds a combined down+up phase for real-world two-way load.
    </p>
  </section>

  <!-- Gauge Scale -->
  <section class="panel">
    <h3>Gauge Scale</h3>
    <Switch
      checked={vizAuto}
      onToggle={setVizAuto}
      label="Auto throughput ceiling"
    />
    <label>
      <span>Throughput max {store.unitLabel}</span>
      <input
        type="number"
        min="1"
        step="1"
        disabled={vizAuto}
        value={vizAuto ? "" : Number(vizDisplay.toFixed(2))}
        oninput={onVizInput}
      />
    </label>
    <p class="hint">
      Manual Y-axis ceiling for the gauge and chart; auto self-scales to the
      peak.
    </p>
  </section>

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

  <!-- "Tuning" tier: set once, rarely revisited. -->
  <h2 class="tier-label">Tuning</h2>

  <!-- Early Finish -->
  <section class="panel">
    <h3>Early Finish</h3>
    <Switch
      disabled={running}
      bind:checked={store.config.adaptive.enabled}
      label="Enable adaptive early phase completion"
    />
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
    <p class="hint">
      Finishes early once the reading stabilizes, instead of running full
      duration.
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

  <!-- Wire-Rate Estimates (wide) -->
  <section class="panel wide">
    <h3>Wire-Rate Estimates</h3>
    <Switch
      bind:checked={store.showWireEstimates}
      label="Include wire-rate estimates in result cards"
      tooltip={JARGON.wireRate}
    />
    <p class="hint">
      Estimates the real wire-rate under measured protocol overhead.
    </p>

    <details class="advanced top-level">
      <summary>Customize the compensation model</summary>
      <div class="disclosure-body">
        <Switch
          disabled={running}
          bind:checked={store.config.compensation.enabled}
          label="Enable overhead compensation model"
          tooltip={JARGON.overheadCompensation}
        />
        <!-- Connection profile + transport presets: seed the factors/params below. -->
        <div class="two">
          <label>
            <span use:tooltip={JARGON.compProfile}>Connection profile</span>
            <select
              disabled={running || !store.config.compensation.enabled}
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
              disabled={running || !store.config.compensation.enabled}
              bind:value={store.config.compensation.transport}
              onchange={reseedProfile}
            >
              {#each TRANSPORT_OPTIONS as o (o.value)}
                <option value={o.value}>{o.label}</option>
              {/each}
            </select>
          </label>
        </div>
        <div class="toggle-groups">
          {#each COMP_GROUPS as g (g.label)}
            <div class="toggle-group">
              <strong use:tooltip={g.tip}>{g.label}</strong>
              {#each g.toggles as t (t.key)}
                <Switch
                  disabled={running || !store.config.compensation.enabled}
                  bind:checked={store.config.compensation.factors[t.key]}
                  label={t.label}
                />
              {/each}
            </div>
          {/each}
        </div>
        <details class="advanced">
          <summary>Advanced — raw byte accounting</summary>
          <p class="hint">
            Defaults come from the profile/transport above — tweak only for a
            nonstandard MTU or tunnel.
          </p>
          <div class="two">
            <label>
              <span>IP version</span>
              <select
                disabled={running}
                bind:value={store.config.compensation.params.ipVersion}
              >
                <option value={4}>IPv4</option>
                <option value={6}>IPv6</option>
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
</div>

<style>
  .setup-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px;
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

  /* Tier separators — "Run" (touched every session) vs. "Tuning" (set once,
     rarely revisited). Plain heading + reorder, not a nested tab: this is
     still one scrollable screen, just with a visual hierarchy the flat
     7-section list didn't have before. */
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

  .toggle-groups {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 10px;
  }
  .toggle-group {
    display: grid;
    align-content: start;
    gap: 8px;
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    padding: 10px;
  }
  .toggle-group strong {
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.1em;
    text-transform: uppercase;
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
</style>
