<script lang="ts">
  import ServerPills from "./ServerPills.svelte";
  import { tooltip } from "../actions/tooltip";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  const selected = $derived(
    store.serverCatalog?.servers.filter((server) =>
      store.selectedServers.includes(server.id),
    ) ?? [],
  );
  const locked = $derived(store.isRunning || store.preparing);
  const problems = $derived(
    (store.serverCatalog?.servers ?? []).filter(
      (server) =>
        store.selectedServers.includes(server.id) &&
        ((store.serverCatalog?.servers.length ?? 0) > 1 ||
          store.serverReadiness[server.id]?.state === "sign-in") &&
        ["failed", "sign-in"].includes(store.serverReadiness[server.id]?.state),
    ),
  );
</script>

{#if (store.serverCatalog?.servers.length ?? 0) > 1}
  <div class="server-setting">
    <div class="server-heading">
      <strong
        use:tooltip={"Select one to four servers. Every selected server carries throughput traffic."}
        >Servers</strong
      >
      <small>{selected.length} / 4</small>
    </div>
    <ServerPills
      servers={store.serverCatalog!.servers}
      value={store.selectedServers}
      label="Servers to test"
      multiple
      expanded
      disabled={locked}
      disabledIds={store
        .serverCatalog!.servers.filter((server) =>
          selected.length === 1
            ? store.selectedServers.includes(server.id)
            : selected.length >= 4 &&
              !store.selectedServers.includes(server.id),
        )
        .map((server) => server.id)}
      onchange={(id) =>
        controller.applyServers(
          store.selectedServers.includes(id)
            ? store.selectedServers.filter((selected) => selected !== id)
            : [...store.selectedServers, id],
        )}
    />
    {#if selected.length > 1}
      <div class="latency-policy">
        <strong
          use:tooltip={"Choose which servers measure idle and loaded latency. Throughput still uses every selected server."}
          >Latency</strong
        >
        <ServerPills
          servers={selected}
          value={store.latencySelection.mode === "all"
            ? ""
            : store.primaryLatencyServer}
          label="Latency measurement servers"
          aggregate
          aggregateDescription="Measure latency on every selected server"
          disabled={locked}
          onchange={(id) =>
            controller.configureLatency(
              id ? "primary" : "all",
              id || store.primaryLatencyServer,
            )}
        />
      </div>
    {/if}
  </div>
{/if}
{#if store.unresolvedServers.length}
  <div class="selection-notice" role="status">
    <p>The saved selection has changed.</p>
    <button
      type="button"
      disabled={locked}
      onclick={() =>
        controller.applyServers(
          selected.length ? selected.map((server) => server.id) : ["self"],
        )}>Use available servers</button
    >
  </div>
{/if}
{#if !store.serverCatalog}
  <div class="selection-notice" role="status">
    <p>
      {store.catalogLoading ? "Loading servers…" : "Could not load servers."}
    </p>
    <button
      type="button"
      disabled={locked || store.catalogLoading}
      onclick={() => void controller.retryCatalogue()}>Retry servers</button
    >
  </div>
{/if}
{#each problems as server (server.id)}
  <div class="server-feedback" role="status">
    <div>
      <strong>{server.name}</strong>{#if server.location}<small
          >{server.location}</small
        >{/if}
    </div>
    <p>{store.serverReadiness[server.id]?.message}</p>
    {#if store.serverReadiness[server.id]?.state === "sign-in"}
      <button
        type="button"
        disabled={locked ||
          (store.serverApproval?.id === server.id &&
            !store.serverApproval.message)}
        aria-label={`Sign in to ${server.name}`}
        onclick={() => void controller.signInServer(server.id)}
        >{store.serverApproval?.id === server.id &&
        !store.serverApproval.message
          ? "Waiting for approval…"
          : `Sign in to ${server.name}`}</button
      >
    {:else}
      <button
        type="button"
        disabled={locked}
        onclick={() => void controller.retryServer(server.id)}
        >Retry {server.name}</button
      >
    {/if}
    {#if store.serverApproval?.id === server.id}
      <button type="button" onclick={controller.cancelServerApproval}
        >Cancel sign-in</button
      >
      <p>
        Complete sign-in in the other window. <a
          href={store.serverApproval.url}
          target="_blank"
          rel="noopener noreferrer">Open sign-in page</a
        >
        {store.serverApproval.message ?? ""}
      </p>
    {/if}
  </div>
{/each}

<style>
  .server-setting {
    display: grid;
    gap: 8px;
    font: var(--type-sm)/1.4 var(--font-sans);
  }
  .server-heading,
  .latency-policy {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  strong {
    font-size: var(--type-sm);
    font-weight: 600;
  }
  small {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .latency-policy {
    min-height: 34px;
  }
  .server-feedback,
  .selection-notice {
    padding: 10px 0;
    border-top: 1px solid var(--border);
    font: var(--type-xs)/1.5 var(--font-sans);
  }
  .server-feedback > div {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
  }
  p {
    margin: 4px 0;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  button,
  a {
    color: var(--brand-strong);
  }
  button {
    border: 0;
    border-radius: 999px;
    background: var(--brand-soft);
    min-height: 32px;
    padding: 5px 12px;
    font: 600 var(--type-xs)/1.3 var(--font-sans);
    cursor: pointer;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  button:focus-visible {
    outline: 2px solid var(--brand);
    outline-offset: 2px;
  }
</style>
