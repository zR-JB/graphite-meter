<script lang="ts">
  import { store } from "../../state/store.svelte";
  import { validateConnections } from "../../runner/engine.svelte";
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
    locked?: boolean;
  }
  let { role, options, locked = false }: Props = $props();

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
        class="choice"
        class:selected={selected === item.value}
        class:unavailable={view.disabled || locked}
      >
        <input
          type="radio"
          name={`${role}-target`}
          value={item.value}
          checked={selected === item.value}
          disabled={view.disabled || locked}
          onchange={() => select(item.value)}
        />
        <span class="radio-dot" aria-hidden="true"></span>
        <span class="copy">
          <strong>{item.label}</strong>
          <small>{view.detail}</small>
        </span>
      </label>
    {/each}
  </div>
  <div
    class="validation"
    class:error={connection.validation === "failed"}
    aria-live="polite"
  >
    <span class="dot" data-state={connection.validation}></span>
    <span class="validation-copy">
      <strong>{locked ? "In use" : status}</strong>
      <small>{connection.message ?? connection.summary}</small>
    </span>
    {#if !locked && (connection.validation === "failed" || connection.validation === "stale")}
      <button type="button" onclick={() => void validateConnections(true, role)}
        >Retry</button
      >
    {/if}
  </div>
</fieldset>

<style>
  fieldset {
    display: grid;
    gap: 7px;
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
  }
  legend {
    margin-bottom: 7px;
    padding: 0;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .options {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(142px, 1fr));
    gap: 6px;
  }
  .choice {
    position: relative;
    display: grid;
    grid-template-columns: 14px minmax(0, 1fr);
    align-items: center;
    gap: var(--space-2);
    min-height: 52px;
    padding: 8px 9px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    cursor: pointer;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out),
      box-shadow var(--dur-hover) var(--ease-out);
  }
  .choice:hover:not(.unavailable) {
    border-color: color-mix(in srgb, var(--brand) 38%, var(--border));
  }
  .choice.selected {
    border-color: color-mix(in srgb, var(--brand) 62%, var(--border));
    background: var(--brand-soft);
    box-shadow: inset 0 0 0 1px
      color-mix(in srgb, var(--brand) 18%, transparent);
  }
  .choice.unavailable {
    opacity: 0.56;
    cursor: not-allowed;
  }
  .choice input {
    position: absolute;
    width: 1px;
    height: 1px;
    opacity: 0;
    pointer-events: none;
  }
  .choice:focus-within {
    border-color: color-mix(in srgb, var(--brand) 62%, var(--border));
    box-shadow: 0 0 0 3px var(--brand-soft);
  }
  .radio-dot {
    grid-column: 1;
    box-sizing: border-box;
    width: 14px;
    height: 14px;
    border: 1px solid var(--text-soft);
    border-radius: 50%;
  }
  .choice.selected .radio-dot {
    border: 4px solid var(--brand-strong);
    background: var(--surface-1);
  }
  .copy {
    grid-column: 2;
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .copy strong {
    color: var(--text);
    font-size: 11px;
    font-weight: 780;
  }
  .copy small {
    overflow: hidden;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 500;
    line-height: 1.35;
    text-overflow: ellipsis;
  }
  .validation {
    display: grid;
    grid-template-columns: 7px minmax(0, 1fr) auto;
    gap: var(--space-2);
    align-items: center;
    min-height: 28px;
    padding: 2px 3px;
  }
  .validation-copy {
    display: flex;
    min-width: 0;
    gap: 6px;
    align-items: baseline;
  }
  .validation-copy strong {
    flex: none;
    color: var(--text);
    font-size: 10px;
    font-weight: 750;
  }
  .validation-copy small {
    overflow: hidden;
    min-width: 0;
    color: var(--text-soft);
    font-size: 10px;
    line-height: 1.4;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    width: 7px;
    height: 7px;
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
    min-height: 28px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    color: var(--text);
    padding: 4px 9px;
    font-family: var(--font-sans);
    font-size: 10px;
    font-weight: 700;
    cursor: pointer;
  }
  button:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  @container (max-width: 360px) {
    .options {
      grid-template-columns: 1fr;
    }
  }
</style>
