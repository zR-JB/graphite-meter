<script lang="ts">
  import { store } from "../../state/store.svelte";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  import {
    summarizeRoleValidation,
    type ConnectionRole,
  } from "../../runner/connectionModel";
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
  const roleSummary = $derived(
    summarizeRoleValidation(store.config, role, serverIds, store.servers),
  );
  const validation = $derived(
    simultaneous
      ? store.unresolvedServers.length
        ? "failed"
        : roleSummary.state
      : connection.validation,
  );
  const summary = $derived(
    simultaneous
      ? `${roleSummary.verified} of ${roleSummary.total} servers ready. Paths resolve independently.`
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

<fieldset>
  <legend class="caps">{title}</legend>
  <div class="options">
    {#each options as option (option.value)}
      {@const view =
        option.detail !== undefined
          ? { disabled: option.disabled ?? false, detail: option.detail }
          : optionView(option.value)}
      <label
        class="choice"
        class:selected={selected === option.value}
        class:unavailable={view.disabled || locked}
      >
        <input
          type="radio"
          name={`${role}-target`}
          value={option.value}
          checked={selected === option.value}
          disabled={view.disabled || locked}
          onchange={() => select(option.value)}
        />
        <span class="radio-dot" aria-hidden="true"></span>
        <span class="copy">
          <strong>{option.label}</strong>
          <small>{view.detail}</small>
        </span>
      </label>
    {/each}
  </div>
  {#if selected !== "auto" && !locked && (options.find((option) => option.value === selected)?.disabled || validation === "failed")}
    <button class="btn" type="button" onclick={() => select("auto")}
      >Use Automatic</button
    >
  {/if}
  <div
    class="validation"
    class:error={validation === "failed"}
    aria-live="polite"
  >
    <span class="dot" data-state={validation}></span>
    <span class="validation-copy">
      <strong>{locked ? "In use" : status}</strong>
      <small>{summary}</small>
    </span>
    {#if !locked && (validation === "failed" || validation === "stale")}
      <!-- Both pickers mount at once and a <legend> does not name a descendant
           button, so without this the rotor reads "Retry, Retry". -->
      <button
        class="btn"
        type="button"
        aria-label={`Retry ${title}`}
        onclick={() =>
          void controller.validateConnections(true, role).catch(() => {})}
        >Retry</button
      >
    {/if}
  </div>
</fieldset>

<style>
  fieldset {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  legend {
    margin-bottom: 6px;
  }
  .options {
    display: grid;
    gap: 6px;
  }
  /* Settings cards are 180px minimum with a 12px grid gap: 180 + 12 + 180 =
     372px, the exact outer-grid two-column breakpoint. */
  @container settings-grid (min-width: 372px) {
    .options {
      grid-template-columns: repeat(auto-fit, minmax(min(100%, 160px), 1fr));
    }
  }
  .choice {
    position: relative;
    display: grid;
    grid-template-columns: 14px minmax(0, 1fr);
    align-items: center;
    gap: var(--space-2);
    min-height: 52px;
    min-width: 0;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    transition: var(--transition-control);
  }
  @media (hover: hover) {
    .choice:hover:not(.unavailable) {
      border-color: color-mix(in srgb, var(--brand) 38%, var(--border));
    }
  }
  .choice.selected {
    border-color: var(--brand-line);
    background: var(--brand-soft);
  }
  .choice.unavailable {
    opacity: 0.56;
    cursor: not-allowed;
  }
  .choice input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .choice:focus-within {
    border-color: var(--brand-line);
    box-shadow: var(--ring-halo);
  }
  .radio-dot {
    width: 14px;
    height: 14px;
    border: 1px solid var(--text-soft);
    border-radius: var(--r-full);
  }
  .choice.selected .radio-dot {
    border: 4px solid var(--brand-strong);
    background: var(--surface-1);
  }
  .copy {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .copy strong {
    font-size: var(--type-xs);
    font-weight: var(--w-heavy);
    overflow-wrap: anywhere;
  }
  .copy small {
    display: -webkit-box;
    overflow: hidden;
    color: var(--text-soft);
    font: var(--type-2xs) / 1.35 var(--font-mono);
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }
  .btn {
    justify-self: start;
  }
  .validation {
    display: grid;
    grid-template-columns: 7px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    min-height: 28px;
    padding-inline: 3px;
    font-size: var(--type-2xs);
  }
  .validation-copy {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  .validation-copy strong {
    flex: none;
    font-weight: var(--w-heavy);
  }
  .validation-copy small {
    overflow: hidden;
    min-width: 0;
    color: var(--text-soft);
    font-size: inherit;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: var(--r-full);
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
</style>
