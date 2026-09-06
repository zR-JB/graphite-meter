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
  const latencyCount = $derived(
    details.servers.filter((server) => server.latencyTarget).length,
  );
</script>

<div class="result-server-context">
  <div class="scope-heading">
    <span
      >{server ? server.name : "Aggregate"}<small
        >{server
          ? server.location || new URL(server.url).host
          : `${details.participants.length < details.selection.length ? `${details.participants.length} of ${details.selection.length} servers` : `${details.selection.length} servers`} · ${latencyCount === 1 ? "One latency server" : `${latencyCount} latency servers`}`}</small
      ></span
    >
    <ServerPills
      servers={details.selection}
      {value}
      {onchange}
      aggregate
      label="Result measurements"
    />
  </div>
  {#if server}<p>
      Measured during shared load. Server results may use different measurement
      windows.
    </p>{/if}
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
    justify-content: space-between;
    gap: var(--space-2);
  }
  .scope-heading > span {
    display: grid;
    gap: 2px;
    font-weight: 600;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  small,
  p {
    color: var(--text-muted);
    font-size: var(--type-xs);
    font-weight: 400;
  }
  p {
    margin: 0;
  }
  .issues {
    margin: 0;
    padding-left: 16px;
    font-size: var(--type-xs);
    color: var(--warn);
  }
</style>
