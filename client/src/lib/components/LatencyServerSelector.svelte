<script lang="ts">
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
</script>

{#if servers.length > 1}<label class="latency-focus"
    >Latency to <select
      aria-label="Server shown in latency gauge, profile and chart"
      value={store.latencyFocus}
      onchange={(event) => controller.focusServer(event.currentTarget.value)}
      >{#each servers as server (server.id)}<option value={server.id}
          >{server.name}</option
        >{/each}</select
    ></label
  >{/if}

<style>
  .latency-focus {
    display: flex;
    gap: 0.55rem;
    align-items: center;
    font-size: 0.7rem;
    color: var(--text-muted);
    margin: 0.25rem 0 0.6rem;
  }
  select {
    font: inherit;
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    padding: 0.3rem 0.5rem;
    max-width: 75%;
  }
  select:focus-visible {
    outline: 2px solid var(--brand);
    outline-offset: 2px;
  }
</style>
