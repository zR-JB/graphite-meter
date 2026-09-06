<script lang="ts">
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
  <button
    class="server-trigger"
    type="button"
    onclick={controller.openServers}
    disabled={store.isRunning || store.preparing}
    aria-haspopup="dialog"
    aria-label={`Change servers, ${selected.length} selected`}
  >
    <span
      ><small>Servers</small><strong
        >{selected.length === 1
          ? selected[0].name
          : `${selected.length} selected`}</strong
      ></span
    >
    <span class="change">Change <span aria-hidden="true">›</span></span>
  </button>
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
  .server-trigger {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    min-width: 0;
    min-height: 52px;
    padding: var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    color: var(--text);
    font: 500 var(--type-sm)/1.4 var(--font-sans);
    text-align: left;
    cursor: pointer;
  }
  .server-trigger > span:first-child {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  small {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  strong {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .change {
    flex: none;
    color: var(--brand-strong);
  }
  .change span {
    margin-left: 6px;
  }
  .server-trigger:hover:not(:disabled) {
    border-color: var(--brand);
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .selection-notice {
    font: var(--type-sm)/1.5 var(--font-sans);
    color: var(--text-muted);
  }
  .selection-notice button {
    color: var(--brand-strong);
    text-decoration: underline;
    min-height: 36px;
    cursor: pointer;
  }
</style>
