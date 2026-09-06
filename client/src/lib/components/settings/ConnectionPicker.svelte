<script lang="ts">
  import { tooltip } from "../../actions/tooltip";
  import { store } from "../../state/store.svelte";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  const descriptionId = $props.id();
  import type { ConnectionRole } from "../../runner/connectionModel";
  import {
    latencyOptionView,
    throughputOptionView,
  } from "../../runner/real/transportViewModel";

  interface Option {
    value: string;
    label: string;
    disabled?: boolean;
    detail?: string;
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
  const serverIds = $derived(
    role === "latency" && store.latencySelection.mode === "primary"
      ? [store.primaryLatencyServer]
      : store.selectedServers,
  );
  const simultaneous = $derived(serverIds.length > 1);
  const validation = $derived(
    simultaneous ? store.selectionValidation : connection.validation,
  );
  const summary = $derived(
    simultaneous
      ? `${serverIds.filter((id) => store.serverReadiness[id]?.state === "ready").length} of ${serverIds.length} servers ready. Paths resolve independently.`
      : (connection.message ?? connection.summary),
  );
  const title = $derived(
    role === "throughput" ? "Throughput path" : "Latency path",
  );
  const status = $derived(
    validation === "verified"
      ? "Ready"
      : validation[0].toUpperCase() + validation.slice(1),
  );

  function select(value: string) {
    controller.selectConnection(role, value);
  }

  function optionView(value: string) {
    return role === "throughput"
      ? throughputOptionView(store.transportDiscovery, value)
      : latencyOptionView(store.transportDiscovery, value);
  }
</script>

<fieldset aria-label={title}>
  <legend
    ><span>{title}</span>
    <span
      class="path-state"
      data-state={validation}
      use:tooltip={summary}
      aria-label={`${title}: ${locked ? "In use" : status}. ${summary}`}
    >
      <span class="dot" aria-hidden="true"></span>{locked ? "In use" : status}
    </span>
  </legend>
  <div class="options">
    {#each options as option, index (option.value)}
      {@const view =
        option.detail !== undefined
          ? { disabled: option.disabled ?? false, detail: option.detail }
          : optionView(option.value)}
      <label
        class="choice"
        class:selected={selected === option.value}
        class:unavailable={view.disabled || locked}
        use:tooltip={view.detail}
        tabindex="-1"
      >
        <input
          type="radio"
          name={`${role}-target`}
          value={option.value}
          aria-describedby={`${descriptionId}-${index}`}
          checked={selected === option.value}
          disabled={view.disabled || locked}
          onchange={() => select(option.value)}
        />
        <span>{option.label}</span>
      </label>
      <span class="sr-only" id={`${descriptionId}-${index}`}>{view.detail}</span
      >
    {/each}
  </div>
  {#if !locked && (validation === "failed" || validation === "stale" || options.find((option) => option.value === selected)?.disabled)}
    <div class="path-recovery">
      {#if validation === "failed"}<span role="status">{summary}</span>{/if}
      <button
        type="button"
        aria-label={`Retry ${title}`}
        onclick={() =>
          void controller.validateConnections(true, role).catch(() => {})}
        >Recheck</button
      >
      {#if selected !== "auto"}<button
          type="button"
          onclick={() => select("auto")}>Use Automatic</button
        >{/if}
    </div>
  {/if}
</fieldset>

<style>
  fieldset {
    display: grid;
    gap: 6px;
    border: 0;
    min-width: 0;
    margin: 0;
    padding: 10px 0 0;
    border-top: 1px solid var(--border);
  }
  legend {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 0;
    color: var(--text);
    font: 600 var(--type-sm)/1.3 var(--font-sans);
  }
  .path-state {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    white-space: nowrap;
    color: var(--text-muted);
    font: var(--type-xs)/1.3 var(--font-sans);
  }
  .dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--text-muted);
  }
  [data-state="verified"] .dot {
    background: var(--ok);
  }
  [data-state="failed"] .dot {
    background: var(--err);
  }
  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .choice {
    position: relative;
    display: inline-flex;
    align-items: center;
    min-height: 30px;
    padding: 4px 10px;
    border: 1px solid transparent;
    border-radius: 999px;
    background: var(--surface-inset);
    color: var(--text-muted);
    font: 600 var(--type-xs)/1.3 var(--font-sans);
    cursor: pointer;
    transition:
      background 120ms ease,
      border-color 120ms ease;
  }
  .choice:hover:not(.unavailable) {
    background: var(--surface-3);
    color: var(--text);
  }
  .choice.selected {
    color: var(--brand-strong);
    background: var(--brand-soft);
    border-color: color-mix(in srgb, var(--brand) 60%, transparent);
  }
  .choice.unavailable {
    opacity: 0.5;
    cursor: default;
  }
  input {
    position: absolute;
    width: 1px;
    height: 1px;
    opacity: 0;
  }
  .choice:has(input:focus-visible) {
    outline: 2px solid var(--brand);
    outline-offset: 2px;
  }
  .path-recovery {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font: var(--type-xs)/1.4 var(--font-sans);
  }
  .path-recovery > span {
    flex-basis: 100%;
  }
  button {
    min-height: 28px;
    border: 0;
    border-radius: 999px;
    padding: 4px 8px;
    background: var(--surface-inset);
    color: var(--brand-strong);
    font: inherit;
    cursor: pointer;
  }
  button:focus-visible {
    outline: 2px solid var(--brand);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .choice {
      transition: none;
    }
  }
</style>
