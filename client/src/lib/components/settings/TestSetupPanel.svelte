<script lang="ts">
  /* ============================================================
   * <TestSetupPanel> — Settings › Test Setup (§13.6)
   * Every control two-way binds to `console.config` (or the unit
   * display prefs), so edits reflect instantly in the rail ETA,
   * gauge, chart, and unit labels. Inputs that are unsafe to
   * change mid-run are disabled while `running`.
   * ============================================================ */
  import {
    store,
    DURATION_PRESETS,
  } from "../../state/store.svelte";
  import type {
    RunnerConfig,
    ConnectionProfile,
    CompensationTransport,
  } from "../../runner/contract";
  import { applyConnectionProfile } from "../../compensation";
  import { tooltip, JARGON } from "../../actions/tooltip";

  interface Props {
    /** Mirrors store.isRunning — locks mid-run-unsafe inputs. */
    running?: boolean;
  }
  let { running = false }: Props = $props();

  /* ---------- Duration presets (synced with the rail) ---------- */
  type PresetKey = "short" | "medium" | "long" | "custom";
  const PRESET_KEYS: PresetKey[] = ["short", "medium", "long", "custom"];
  type Durations = RunnerConfig["duration"];
  function durationsEqual(a: Durations, b: Durations): boolean {
    return (
      a.warmupMs === b.warmupMs &&
      a.latencyMs === b.latencyMs &&
      a.downloadMs === b.downloadMs &&
      a.uploadMs === b.uploadMs
    );
  }
  function presetFromDurations(): PresetKey {
    for (const k of ["short", "medium", "long"] as const) {
      if (durationsEqual(store.config.duration, DURATION_PRESETS[k])) return k;
    }
    return "custom";
  }
  // Explicit mode (seeded from the loaded durations) so the Custom tab is
  // directly selectable and editing any field switches to Custom — the old
  // value-equality-only derivation left the Custom tab inert.
  let durationMode = $state<PresetKey>(presetFromDurations());

  function applyPreset(k: PresetKey) {
    durationMode = k;
    // Presets apply their durations; Custom keeps the current values for editing.
    if (k !== "custom") store.config.duration = { ...DURATION_PRESETS[k] };
  }
  /** Any manual field edit means we're no longer on a named preset. */
  function onDurationEdit() {
    durationMode = "custom";
  }

  const DUR_FIELDS = [
    { key: "warmupMs", label: "Warmup ms" },
    { key: "latencyMs", label: "Latency ms" },
    { key: "downloadMs", label: "Download ms" },
    { key: "uploadMs", label: "Upload ms" },
  ] as const;

  /* ---------- Visualization throughput max ----------
   * Stored as bytesPerSec (or "auto"); edited in the active display unit. */
  const vizAuto = $derived(store.config.visualization.throughputMaxBytesPerSec === "auto");
  // Display value = bytesPerSec → active unit (matches the gauge/chart label).
  const vizDisplay = $derived(
    vizAuto ? 0 : store.toUnit(store.config.visualization.throughputMaxBytesPerSec as number),
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
      // Seed a sane manual ceiling from the current display value (or 1000 unit).
      const seed = vizAuto ? 1000 : vizDisplay;
      store.config.visualization.throughputMaxBytesPerSec = toBytesPerSec(seed);
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
  const COMP_GROUPS: { label: string; tip: string; toggles: { key: FactorKey; label: string }[] }[] = [
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
    { key: "tcpOptionsBytes", label: "TCP options B", min: 0, max: 40, step: 4 },
    { key: "encapsulationBytes", label: "Encapsulation B", min: 0, max: 128, step: 1 },
    { key: "framePayloadBytes", label: "Frame payload B", min: 256, max: 65536, step: 256 },
    { key: "tlsRecordBytes", label: "TLS record B", min: 0, max: 64, step: 1 },
    { key: "aeadTagBytes", label: "AEAD tag B", min: 0, max: 255, step: 1 },
    { key: "quicConnIdBytes", label: "QUIC CID B", min: 0, max: 20, step: 1 },
    { key: "maxLossRatio", label: "Max loss ratio", min: 0, max: 1, step: 0.01 },
  ] as const;
</script>

<div class="setup-grid">
  <!-- Duration -->
  <section class="panel">
    <h3>Duration Strategy</h3>
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
    <div class="two">
      {#each DUR_FIELDS as f (f.key)}
        <label>
          <span>{f.label}</span>
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
    <p class="hint">Pick a preset, or edit any field to switch to Custom.</p>
  </section>

  <!-- Adaptive -->
  <section class="panel">
    <h3>Adaptive Measurement</h3>
    <label class="check-row">
      <input
        type="checkbox"
        disabled={running}
        bind:checked={store.config.adaptive.enabled}
      />
      <span>Enable adaptive early phase completion</span>
    </label>
    <div class="two">
      <label>
        <span>Min coverage</span>
        <input type="number" min="0.25" max="1" step="0.01" disabled={running}
          bind:value={store.config.adaptive.minCoverageRatio} />
      </label>
      <label>
        <span use:tooltip={JARGON.stability}>Stability</span>
        <input type="number" min="0.5" max="0.99" step="0.01" disabled={running}
          bind:value={store.config.adaptive.stabilityThreshold} />
      </label>
      <label>
        <span>Glide (ms)</span>
        <input type="number" min="300" max="1500" step="50" disabled={running}
          bind:value={store.config.adaptive.glideMs} />
      </label>
    </div>
    <p class="hint">Once stable, the marker accelerates and glides to the phase end — the test finishes sooner while still measuring. Off → every phase runs full and reports its whole-phase average.</p>
  </section>

  <!-- Advanced stages -->
  <section class="panel">
    <h3>Advanced Stages</h3>
    <label class="check-row">
      <input
        type="checkbox"
        disabled={running}
        bind:checked={store.config.stages.bidirectional}
      />
      <span>Bidirectional (concurrent down + up)</span>
    </label>
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
      Appends a final phase that saturates download and upload at once — a truer
      test of a link under simultaneous two-way load. The result card reports the
      combined and per-direction rates. Off by default.
    </p>
  </section>

  <!-- Engine -->
  <section class="panel">
    <h3>Engine</h3>
    <label class="check-row">
      <input
        type="checkbox"
        disabled={running}
        bind:checked={store.config.experimentalChunkedDownload}
      />
      <span>Chunked download (experimental)</span>
    </label>
    <p class="hint">
      Request adaptively-sized download chunks instead of one long stream per
      lane — both target ~350&nbsp;ms on the same keep-alive connection. A/B
      against the default to compare ramp responsiveness on a real line.
    </p>
  </section>

  <!-- Timing / streams -->
  <section class="panel">
    <h3>Timing / Streams</h3>
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
      <input type="number" min="1" max="6" step="1" disabled={running}
        bind:value={store.config.parallelStreams} />
    </label>
    <p class="hint">Lanes are chosen automatically per phase; this only caps the maximum.</p>
    <label class="check-row">
      <input
        type="checkbox"
        disabled={running}
        bind:checked={store.config.skipLoadedLatencyWhenStageOff}
      />
      <span>Skip loaded-latency when the latency stage is off</span>
    </label>
    <p class="hint">
      Turning the latency stage off also drops the under-load pings during
      download/upload — no latency profile or chart line. Off keeps measuring
      bufferbloat even with the idle latency stage disabled.
    </p>
  </section>

  <!-- Visualizer -->
  <section class="panel">
    <h3>Visualizer</h3>
    <label class="check-row">
      <input type="checkbox" checked={vizAuto} onchange={(e) => setVizAuto((e.currentTarget as HTMLInputElement).checked)} />
      <span>Auto throughput ceiling</span>
    </label>
    <label>
      <span>Throughput max {store.unitLabel}</span>
      <input type="number" min="1" step="1" disabled={vizAuto}
        value={vizAuto ? "" : Number(vizDisplay.toFixed(2))} oninput={onVizInput} />
    </label>
    <p class="hint">Manual Y-axis ceiling for the gauge and chart; auto self-scales to the peak.</p>
  </section>

  <!-- Overhead compensation (wide) -->
  <section class="panel wide">
    <h3>Overhead Compensation</h3>
    <label class="check-row">
      <input type="checkbox" bind:checked={store.showWireEstimates} />
      <span use:tooltip={JARGON.wireRate}>Include wire-rate estimates in result cards</span>
    </label>
    <label class="check-row">
      <input type="checkbox" disabled={running} bind:checked={store.config.compensation.enabled} />
      <span use:tooltip={JARGON.overheadCompensation}>Show estimated wire-rate compensation</span>
    </label>
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
        <span use:tooltip={JARGON.compTransport}>Transport &amp; security</span>
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
            <label class="check-row">
              <input
                type="checkbox"
                disabled={running || !store.config.compensation.enabled}
                bind:checked={store.config.compensation.factors[t.key]}
              />
              <span>{t.label}</span>
            </label>
          {/each}
        </div>
      {/each}
    </div>
    <details class="advanced">
      <summary>Advanced — raw byte accounting</summary>
      <p class="hint">
        Sane defaults come from the profile + transport above; tweak these only to
        match a non-standard MTU, jumbo frames, or a custom tunnel/record size.
      </p>
      <div class="two">
        <label>
          <span>IP version</span>
          <select disabled={running} bind:value={store.config.compensation.params.ipVersion}>
            <option value={4}>IPv4</option>
            <option value={6}>IPv6</option>
          </select>
        </label>
        {#each COMP_NUMS as n (n.key)}
          <label>
            <span>{n.label}</span>
            <input type="number" min={n.min} max={n.max} step={n.step} disabled={running}
              bind:value={store.config.compensation.params[n.key]} />
          </label>
        {/each}
        <label class="check-row spanned">
          <input type="checkbox" disabled={running} bind:checked={store.config.compensation.params.vlanTagged} />
          <span>VLAN tagged (+4B/frame)</span>
        </label>
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
    background: linear-gradient(180deg, var(--surface-2), transparent),
      var(--surface-inset);
    padding: var(--space-3);
    box-shadow: var(--elev-recess);
  }
  .panel.wide {
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

  label {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  label span {
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

  /* Checkbox rows */
  .check-row {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr);
    align-items: center;
    gap: 9px;
  }
  .check-row.spanned {
    grid-column: 1 / -1;
  }
  .check-row input[type="checkbox"] {
    appearance: none;
    -webkit-appearance: none;
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    background: var(--surface-1);
    padding: 0;
    cursor: pointer;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out),
      box-shadow var(--dur-hover) var(--ease-out);
  }
  .check-row input[type="checkbox"]:checked {
    border-color: color-mix(in srgb, var(--brand) 72%, var(--border-strong));
    background:
      radial-gradient(circle at center, var(--brand-strong) 0 42%, transparent 45%),
      var(--surface-1);
    box-shadow: 0 0 0 3px var(--brand-soft);
  }
  .check-row input[type="checkbox"]:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
  }
  .check-row input[type="checkbox"]:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .check-row span {
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0;
    text-transform: none;
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

  /* Advanced disclosure — keeps the raw byte knobs out of the way until needed. */
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
</style>
