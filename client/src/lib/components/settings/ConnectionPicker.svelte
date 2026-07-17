<script lang="ts">
  import { store } from "../../state/store.svelte";
  import { validateConnections } from "../../runner/wire.svelte";
  import type { ConnectionRole } from "../../runner/connectionModel";
  import {
    latencyOptionView,
    throughputOptionView,
  } from "../../runner/real/transportViewModel";

  interface Option {
    value: string;
    label: string;
  }
  interface Props {
    role: ConnectionRole;
    options: readonly Option[];
    running?: boolean;
  }
  let { role, options, running = false }: Props = $props();

  const selected = $derived(
    role === "throughput"
      ? store.config.transports.throughputTarget
      : store.config.transports.latencyTarget,
  );
  const connection = $derived(store.connections[role]);
  const title = $derived(
    role === "throughput" ? "Throughput path" : "Latency path",
  );
  const status = $derived(
    connection.validation === "verified"
      ? "Ready"
      : connection.validation[0].toUpperCase() + connection.validation.slice(1),
  );

  function select(value: string) {
    if (role === "throughput") store.config.transports.throughputTarget = value;
    else store.config.transports.latencyTarget = value;
  }

  function option(value: string) {
    return role === "throughput"
      ? throughputOptionView(store.transportDiscovery, value)
      : latencyOptionView(store.transportDiscovery, value);
  }
</script>

<fieldset>
  <legend>{title}</legend>
  <div class="options">
    {#each options as item (item.value)}
      {@const view = option(item.value)}
      <label
        class:selected={selected === item.value}
        class:unavailable={view.disabled}
      >
        <input
          type="radio"
          name={`${role}-target`}
          value={item.value}
          checked={selected === item.value}
          disabled={view.disabled}
          onchange={() => select(item.value)}
        />
        <span>
          <strong>{item.label}</strong>
          <small>{view.detail}</small>
        </span>
      </label>
    {/each}
  </div>
  <div
    class="status"
    class:error={connection.validation === "failed"}
    aria-live="polite"
  >
    <span class="dot" data-state={connection.validation}></span>
    <span>
      <strong>{status}</strong>
      <small>{connection.message ?? connection.summary}</small>
      {#if running}<small
          >Edits affect an unconsumed path or the next run.</small
        >{/if}
    </span>
    {#if connection.validation === "failed" || connection.validation === "stale"}
      <button type="button" onclick={() => void validateConnections(true)}
        >Retry</button
      >
    {/if}
  </div>
</fieldset>

<style>
  fieldset {
    display: grid;
    gap: 8px;
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
  }
  legend {
    margin-bottom: 7px;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
  .options {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(135px, 1fr));
    gap: 6px;
  }
  label {
    display: grid;
    grid-template-columns: 14px minmax(0, 1fr);
    gap: 8px;
    align-items: center;
    min-height: 48px;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    cursor: pointer;
  }
  label.selected {
    border-color: color-mix(in srgb, var(--brand) 62%, var(--border));
    background: var(--brand-soft);
  }
  label.unavailable {
    opacity: 0.5;
    cursor: not-allowed;
  }
  label span,
  .status span {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  strong {
    font-size: 11px;
  }
  small {
    color: var(--text-soft);
    font-size: 9px;
    line-height: 1.35;
  }
  .status {
    display: grid;
    grid-template-columns: 8px minmax(0, 1fr) auto;
    gap: 8px;
    align-items: center;
    min-height: 34px;
    padding: 7px 9px;
    border-radius: var(--radius-sm);
    background: var(--surface-inset);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-soft);
  }
  .dot[data-state="verified"] {
    background: var(--ok);
  }
  .dot[data-state="checking"] {
    background: var(--brand);
  }
  .dot[data-state="failed"] {
    background: var(--warn);
  }
  button {
    min-height: 30px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--text);
    cursor: pointer;
  }
</style>
