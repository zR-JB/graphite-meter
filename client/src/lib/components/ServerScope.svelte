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
  @media (pointer: coarse) {
    .server-scope {
      min-height: var(--hit);
    }
  }
</style>
