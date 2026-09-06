<script lang="ts">
  import ServerPills from "./ServerPills.svelte";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  const servers = $derived(
    store.serverDetails?.selection ??
      store.serverCatalog?.servers.filter((server) =>
        store.selectedServers.includes(server.id),
      ) ??
      [],
  );
  const measured = $derived(
    store.serverDetails
      ? servers.filter((server) =>
          store.serverDetails!.servers.some(
            (result) => result.server.id === server.id && result.latencyTarget,
          ),
        )
      : store.latencySelection.mode === "primary"
        ? servers.filter((server) => server.id === store.primaryLatencyServer)
        : servers,
  );
  const focused = $derived(
    measured.find((server) => server.id === store.latencyFocus) ?? measured[0],
  );
</script>

{#if servers.length > 1 && focused}
  <div class="latency-focus">
    <span>Latency to <strong>{focused.name}</strong></span>
    {#if measured.length > 1}<ServerPills
        {servers}
        value={store.latencyFocus}
        label="Latency server shown in gauge, profile and chart"
        disabledIds={servers
          .filter((server) => !measured.includes(server))
          .map((server) => server.id)}
        onchange={controller.focusServer}
      />{/if}
  </div>
{/if}

<style>
  .latency-focus {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    margin-bottom: var(--space-2);
    color: var(--text-muted);
    font: var(--type-xs)/1.4 var(--font-sans);
  }
  strong {
    color: var(--text-soft);
    font-weight: 600;
  }
</style>
