<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { ServerIdentity } from "../servers/catalog";
  import { serverLabel } from "../presentation/serverAppearance";

  let {
    servers,
    value,
    onchange,
    label,
    aggregate,
    disabled = false,
    disabledIds = [],
    quiet = false,
  }: {
    servers: readonly ServerIdentity[];
    value: string;
    onchange: (id: string) => void;
    label: string;
    aggregate?: string;
    disabled?: boolean;
    disabledIds?: readonly string[];
    /** A lens over results: a server mark and a borderless field. */
    quiet?: boolean;
  } = $props();
</script>

<span class:lens={quiet}>
  {#if quiet}<span class="lens-mark"><Icon name="server" /></span>{/if}
  <select
    class="server-scope"
    class:quiet
    aria-label={label}
    {value}
    {disabled}
    onchange={(event) => onchange(event.currentTarget.value)}
  >
    {#if aggregate}<option value="">{aggregate}</option>{/if}
    {#each servers as server (server.id)}
      <option value={server.id} disabled={disabledIds.includes(server.id)}
        >{serverLabel(server)}</option
      >
    {/each}
  </select>
</span>

<style>
  .lens {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .server-scope {
    max-width: 100%;
    text-overflow: ellipsis;
  }
  .lens-mark {
    display: inline-grid;
    flex: none;
    color: var(--text-soft);
  }
  .lens-mark :global(svg) {
    width: 14px;
    height: 14px;
  }
  /* Sized to the shown choice, so the chevron stays by it however long another server's name is. */
  .quiet {
    field-sizing: content;
    border-color: transparent;
    background-color: transparent;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-body) / 1 var(--font-sans);
  }
  .quiet:focus-visible {
    border-color: var(--brand-line);
  }
  @media (hover: hover) {
    .quiet:hover:not(:disabled) {
      background-color: var(--hover-wash);
    }
  }
</style>
