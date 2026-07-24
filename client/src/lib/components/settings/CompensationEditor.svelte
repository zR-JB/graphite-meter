<script lang="ts">
  import type {
    ConnectionProfile,
    CompensationTransportSetting,
    OverheadCompensationConfig,
  } from "../../runner/contract";
  import { applyConnectionProfile } from "../../compensation";
  import { compensationTransportLabel } from "../../runner/protocol";
  import { tooltip, JARGON } from "../../actions/tooltip";
  import Switch from "../Switch.svelte";

  interface Props {
    /** Edited in place; the caller passes its own reactive config object. */
    compensation: OverheadCompensationConfig;
  }
  let { compensation }: Props = $props();

  const PROFILES: { value: ConnectionProfile; label: string }[] = [
    { value: "lan", label: "Local Ethernet" },
    { value: "loopback", label: "Loopback" },
    { value: "tunnel", label: "VPN / tunnel" },
    { value: "custom", label: "Custom" },
  ];
  const TRANSPORTS: {
    value: CompensationTransportSetting;
    label: string;
  }[] = (
    ["auto", "http1-clear", "https-tls", "http2", "http3-quic"] as const
  ).map((value) => ({
    value,
    label: compensationTransportLabel(value),
  }));
  const NUMBER_FIELDS = [
    ["mtuBytes", "MTU bytes", 576, 65536, 1],
    ["tcpOptionsMinBytes", "TCP options min", 0, 40, 4],
    ["tcpOptionsMaxBytes", "TCP options max", 0, 40, 4],
    ["encapsulationBytes", "Tunnel overhead", 0, 256, 1],
    ["quicConnIdMinBytes", "QUIC CID min", 0, 20, 1],
    ["quicConnIdMaxBytes", "QUIC CID max", 0, 20, 1],
  ] as const;
  // Every profile preset hardcodes IPv4; the IP version is a separate user override.
  function reseedProfile() {
    const preservedIpVersion = compensation.params.ipVersion;
    const preset = applyConnectionProfile(compensation.profile);
    Object.assign(compensation.params, preset.params, {
      ipVersion: preservedIpVersion,
    });
  }
</script>

<details class="advanced top-level">
  <summary>Customize the compensation model</summary>
  <div class="disclosure-body">
    <div class="two">
      <label>
        <span use:tooltip={JARGON.compProfile}>Connection profile</span>
        <select bind:value={compensation.profile} onchange={reseedProfile}>
          {#each PROFILES as option}<option value={option.value}
              >{option.label}</option
            >{/each}
        </select>
      </label>
      <label>
        <span>Transport override</span>
        <select bind:value={compensation.transport}>
          {#each TRANSPORTS as option}<option value={option.value}
              >{option.label}</option
            >{/each}
        </select>
      </label>
    </div>
    <details class="advanced">
      <summary>Advanced — raw byte accounting</summary>
      <div class="disclosure-body nested">
        <label>
          <span>IP version</span>
          <select bind:value={compensation.params.ipVersion}>
            <option value="auto">Automatic</option>
            <option value={4}>IPv4 override</option>
            <option value={6}>IPv6 override</option>
          </select>
        </label>
        <div class="fields">
          {#each NUMBER_FIELDS as [key, label, min, max, step]}
            <label
              ><span>{label}</span><input
                type="number"
                {min}
                {max}
                {step}
                bind:value={compensation.params[key]}
              /></label
            >
          {/each}
        </div>
        <Switch
          bind:checked={compensation.params.vlanTagged}
          label="VLAN tagged (+4B/frame)"
        />
      </div>
    </details>
  </div>
</details>

<style>
  label {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  label > span {
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
  .two,
  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
    gap: 10px;
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
  /* The measured container is the parent TestSetupPanel's .setup-grid
     (container-type: inline-size); this component declares none itself. */
  @container (max-width: 360px) {
    .two {
      grid-template-columns: 1fr;
      gap: var(--space-1);
    }
  }
</style>
