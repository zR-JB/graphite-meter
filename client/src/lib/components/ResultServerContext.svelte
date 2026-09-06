<script lang="ts">
  import type { MultiServerResult } from "../servers/measurement";
  import ServerPills from "./ServerPills.svelte";
  let {
    details,
    value,
    onchange,
  }: {
    details: MultiServerResult;
    value: string;
    onchange: (id: string) => void;
  } = $props();
  const server = $derived(
    details.selection.find((server) => server.id === value),
  );
  const failures = $derived(
    details.failures.filter(
      (failure) => !server || failure.serverId === server.id,
    ),
  );
</script>

<div class="result-server-context">
  <div class="scope-heading">
    {#if details.participants.length < details.selection.length}<span
        class="scope-caption"
        >{details.participants.length} of {details.selection.length} servers</span
      >{/if}
    <ServerPills
      servers={details.selection}
      {value}
      {onchange}
      aggregate
      label="Result measurements"
    />
  </div>
  {#if failures.length}<ul class="issues">
      {#each failures as failure}<li>
          {details.selection.find((server) => server.id === failure.serverId)
            ?.name}: {failure.stage}{failure.scope === "latency"
            ? " latency"
            : ""} — {failure.message}
        </li>{/each}
    </ul>{/if}
</div>

<style>
  .result-server-context {
    display: grid;
    gap: var(--space-2);
    color: var(--text-soft);
    font: var(--type-sm)/1.4 var(--font-sans);
  }
  .scope-heading {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
  }
  .scope-caption {
    color: var(--text-muted);
    font-size: var(--type-xs);
    font-weight: 500;
  }
  .issues {
    margin: 0;
    padding-left: 16px;
    font-size: var(--type-xs);
    color: var(--warn);
  }
</style>
