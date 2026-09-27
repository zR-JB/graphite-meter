<script lang="ts">
  import type { ServerIdentity } from "../servers/catalog";
  import { serverLabel } from "../presentation/serverAppearance";

  let {
    servers,
    value,
    onchange,
    label,
    aggregate,
    hint,
    disabled = false,
    disabledIds = [],
  }: {
    servers: readonly ServerIdentity[];
    value: string;
    onchange: (id: string) => void;
    label: string;
    aggregate?: string;
    hint?: string;
    disabled?: boolean;
    disabledIds?: readonly string[];
  } = $props();
  const hintId = $props.id();
</script>

<select
  class="server-scope"
  aria-label={label}
  aria-describedby={hint && value === "" ? hintId : undefined}
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
{#if hint}<span id={hintId} hidden>{hint}</span>{/if}

<style>
  .server-scope {
    width: var(--scope-width, auto);
    max-width: 100%;
    min-height: var(--control-h);
    padding-block: 5px;
    font: var(--w-strong) var(--type-xs) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
  }
  @supports (appearance: base-select) {
    .server-scope,
    .server-scope::picker(select) {
      appearance: base-select;
    }
    .server-scope::picker(select) {
      margin-block: 4px;
      padding: var(--space-1);
      border: 1px solid var(--border-strong);
      border-radius: var(--r-chrome);
      background: var(--surface-1);
      box-shadow: var(--elev-float);
    }
    option {
      padding: 6px var(--space-2);
      border-radius: var(--r-well);
      font: var(--w-strong) var(--type-xs) / 1.3 var(--font-sans);
    }
    option:checked {
      background: var(--brand-soft);
      color: var(--brand-strong);
    }
  }
  @media (pointer: coarse) {
    .server-scope {
      min-height: var(--hit);
    }
  }
</style>
