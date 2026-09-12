<script lang="ts">
  import type { ServerIdentity } from "../servers/catalog";
  import { serverLabel } from "../presentation/serverAppearance";

  let {
    servers,
    value,
    onchange,
    label,
    aggregate = false,
    disabled = false,
    disabledIds = [],
    aggregateLabel = "All servers",
    aggregateDescription = "Combined speed",
  }: {
    servers: readonly ServerIdentity[];
    value: string;
    onchange: (id: string) => void;
    label: string;
    aggregate?: boolean;
    disabled?: boolean;
    disabledIds?: readonly string[];
    aggregateLabel?: string;
    aggregateDescription?: string;
  } = $props();
  const descriptionId = $props.id();
</script>

<select
  class="server-selector"
  aria-label={label}
  aria-describedby={aggregate && value === "" ? descriptionId : undefined}
  {value}
  {disabled}
  onchange={(event) => onchange(event.currentTarget.value)}
>
  {#if aggregate}
    <option value="" disabled={disabledIds.includes("")}
      >{aggregateLabel}</option
    >
  {/if}
  {#each servers as server (server.id)}
    <option value={server.id} disabled={disabledIds.includes(server.id)}>
      {serverLabel(server)}
    </option>
  {/each}
</select>
{#if aggregate}<span id={descriptionId} hidden>{aggregateDescription}</span
  >{/if}

<style>
  .server-selector {
    width: var(--selector-width, auto);
    min-width: 0;
    max-width: 100%;
    height: 32px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    color: var(--text);
    font: 600 var(--type-xs)/1.3 var(--font-sans);
    cursor: pointer;
    transition: border-color 160ms ease;
  }
  .server-selector:hover:not(:disabled) {
    border-color: var(--border-strong);
  }
  .server-selector:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  .server-selector:disabled {
    color: var(--text-soft);
    cursor: default;
  }
  @media (pointer: coarse) {
    .server-selector {
      height: 44px;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .server-selector {
      transition: none;
    }
  }
</style>
