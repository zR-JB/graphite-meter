<script lang="ts">
  import ServerPills from "./ServerPills.svelte";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  const controller = getApplicationController();
  const selected = $derived(
    store.serverCatalog?.servers.filter((server) =>
      store.selectedServers.includes(server.id),
    ) ?? [],
  );
</script>

{#if (store.serverCatalog?.servers.length ?? 0) > 1}
  <div class="server-setting">
    <div class="server-heading">
      <strong>Servers <small>{selected.length} selected</small></strong>
      <button
        class="change"
        type="button"
        onclick={controller.openServers}
        disabled={store.isRunning || store.preparing}
        aria-haspopup="dialog"
        aria-label={`Change servers, ${selected.length} selected`}
        >Change</button
      >
    </div>
    {#if store.serverCatalog && store.serverCatalog.servers.length <= 8}
      <ServerPills
        servers={store.serverCatalog.servers}
        value={store.selectedServers}
        label="Servers to test"
        multiple
        expanded
        disabled={store.isRunning || store.preparing}
        disabledIds={store.serverCatalog.servers
          .filter((server) =>
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
    {:else}<p>{selected.map((server) => server.name).join(", ")}</p>{/if}
    {#if store.selectionValidation === "failed"}<button
        class="review"
        type="button"
        onclick={controller.openServers}
        disabled={store.isRunning || store.preparing}
        >Review connection issues</button
      >{/if}
    {#if selected.length > 1}
      <div class="latency-policy">
        <strong>Measure latency</strong>
        <div
          class="policy-options"
          role="group"
          aria-label="Latency measurement scope"
        >
          <button
            type="button"
            aria-pressed={store.latencySelection.mode === "primary"}
            disabled={store.isRunning || store.preparing}
            onclick={() => controller.configureLatency("primary")}
            >One server</button
          >
          <button
            type="button"
            aria-pressed={store.latencySelection.mode === "all"}
            disabled={store.isRunning || store.preparing}
            onclick={() => controller.configureLatency("all")}
            >Every server</button
          >
        </div>
        {#if store.latencySelection.mode === "primary"}
          <ServerPills
            servers={selected}
            value={store.primaryLatencyServer}
            label="Primary latency server"
            expanded
            disabled={store.isRunning || store.preparing}
            onchange={(id) => controller.configureLatency("primary", id)}
          />
          <p>
            Idle and loaded latency use this server. Speed tests use all
            selected servers.
          </p>
        {:else}<p>Each server keeps its own latency measurements.</p>{/if}
      </div>
    {/if}
    {#if store.isRunning || store.preparing}<p>
        Server choices are fixed for this run. Display controls remain
        available.
      </p>{/if}
  </div>
{:else if store.unresolvedServers.length}
  <div class="selection-notice">
    <p>The saved selection is no longer available.</p>
    <button
      type="button"
      disabled={store.isRunning || store.preparing}
      onclick={() => controller.applyServers(["self"])}>Use this server</button
    >
  </div>
{/if}

<style>
  .server-setting {
    display: grid;
    gap: var(--space-2);
    font: var(--type-sm)/1.4 var(--font-sans);
  }
  .server-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }
  strong {
    font-weight: 600;
  }
  small {
    margin-left: 6px;
    color: var(--text-muted);
    font-weight: 400;
  }
  p {
    margin: 0;
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .change,
  .review,
  .selection-notice button {
    color: var(--brand-strong);
    min-height: 32px;
    padding: 4px 6px;
    font: inherit;
    cursor: pointer;
    background: transparent;
    border: 0;
  }
  .review {
    justify-self: start;
    text-decoration: underline;
  }
  .latency-policy {
    display: grid;
    gap: var(--space-2);
    padding-block: var(--space-2);
  }
  .policy-options {
    display: flex;
    gap: 4px;
  }
  .policy-options button {
    min-height: 32px;
    padding: 5px 10px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    color: var(--text-muted);
    font: inherit;
    cursor: pointer;
  }
  .policy-options button[aria-pressed="true"] {
    background: var(--surface-2);
    border-color: var(--border-strong);
    color: var(--brand-strong);
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
