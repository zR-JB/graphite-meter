<script lang="ts">
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
  }: {
    servers: readonly ServerIdentity[];
    value: string;
    onchange: (id: string) => void;
    label: string;
    aggregate?: string;
    disabled?: boolean;
    disabledIds?: readonly string[];
  } = $props();
</script>

<select
  class="server-scope"
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

<style>
  .server-scope {
    width: var(--scope-width, auto);
    max-width: 100%;
    text-overflow: ellipsis;
  }
</style>
