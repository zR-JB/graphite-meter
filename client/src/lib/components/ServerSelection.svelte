<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import { fmtMs } from "../format";
  import ServerSelector from "./ServerSelector.svelte";
  import { serverAccent, serverLabel } from "../presentation/serverAppearance";
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
          store.serverReadiness.get(server.id)?.state === "sign-in") &&
        ["failed", "sign-in"].includes(
          store.serverReadiness.get(server.id)?.state ?? "unchecked",
        ),
    ),
  );
</script>

{#if (store.serverCatalog?.servers.length ?? 0) > 1}
  <div class="server-setting">
    <div class="server-heading">
      <strong>Test servers</strong>
      <small>{selected.length} selected</small>
    </div>
    <div
      class="server-choices"
      role="group"
      aria-label="Servers to test"
      aria-busy={store.serverMetadataLoading}
    >
      {#each store.serverCatalog!.servers as server (server.id)}
        {@const checked = store.selectedServers.includes(server.id)}
        {@const preflightMs = store.serverDiscoveries.get(
          server.id,
        )?.preflightMs}
        {@const unavailable =
          locked || (checked ? selected.length === 1 : selected.length >= 4)}
        <label
          tabindex="-1"
          use:tooltip={[server.name, server.location, new URL(server.url).host]
            .concat(
              preflightMs == null
                ? []
                : [
                    "Preflight request time includes connection setup and the response. It is not steady-state ping.",
                  ],
            )
            .filter(Boolean)
            .join("\n")}
          class:checked
          style:--server-accent={serverAccent(
            server,
            store.serverCatalog!.servers,
          )}
        >
          <input
            type="checkbox"
            {checked}
            disabled={unavailable}
            aria-label={[server.name, server.location, new URL(server.url).host]
              .filter(Boolean)
              .join(", ")}
            onchange={() =>
              controller.applyServers(
                checked
                  ? store.selectedServers.filter((id) => id !== server.id)
                  : [...store.selectedServers, server.id],
              )}
          />
          <span class="server-name">{serverLabel(server)}</span>
          {#if preflightMs != null}<small
              class="server-preflight"
              aria-label={`Preflight request ${fmtMs(preflightMs)} milliseconds`}
              >{fmtMs(preflightMs)}<span>ms</span></small
            >{/if}
        </label>
      {/each}
    </div>
    <p class="selection-help">Choose up to 4. Their speeds are combined.</p>
    {#if selected.length > 1 && store.latencyEnabled}
      <div class="latency-policy">
        <strong>Measure ping to</strong>
        <ServerSelector
          servers={selected}
          value={store.latencySelection.mode === "all"
            ? ""
            : store.primaryLatencyServer}
          label="Latency measurement servers"
          aggregate
          aggregateDescription="Ping each server"
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
    <p>{store.serverReadiness.get(server.id)?.message}</p>
    {#if store.serverReadiness.get(server.id)?.state === "sign-in"}
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
      {#if store.serverApproval.renewUrl}
        <p>
          <a
            href={store.serverApproval.renewUrl}
            target="_blank"
            rel="noopener noreferrer">Renew login at {server.name}</a
          >. Renewing ends the other client connections authorized by that
          login. Then choose Sign in again here.
        </p>
      {:else}
        <p class="approval-code">
          Compare this code with the sign-in page:
          <strong aria-label={`Verification code ${store.serverApproval.code}`}
            >{store.serverApproval.code}</strong
          >
          Approve only if both codes match.
        </p>
      {/if}
      <button type="button" onclick={controller.cancelServerApproval}
        >Cancel sign-in</button
      >
      {#if !store.serverApproval.renewUrl}
        <p>
          Complete sign-in in the other window. <a
            href={store.serverApproval.url}
            target="_blank"
            rel="noopener noreferrer">Open sign-in page</a
          >
          {store.serverApproval.message ?? ""}
        </p>
      {/if}
    {/if}
  </div>
{/each}

<style>
  .approval-code strong {
    display: block;
    padding-block: 4px;
    font: 600 var(--type-lg)/1.4 var(--font-mono);
    letter-spacing: 0.12em;
  }
  .server-setting {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-width: 0;
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
    --selector-width: 100%;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-width: 0;
    width: 100%;
    justify-items: start;
    gap: 6px;
    padding-top: 4px;
  }
  .server-choices {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 120px), 1fr));
    gap: 2px;
    max-height: 220px;
    overflow-y: auto;
    padding: 3px;
    border: 1px solid var(--border);
    border-radius: calc(var(--r-well) + 4px);
    background: var(--surface-inset);
  }
  .server-choices label {
    display: grid;
    grid-template-columns: 14px minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
    min-width: 0;
    min-height: 32px;
    padding: 6px 8px;
    border: 1px solid transparent;
    border-radius: var(--r-well);
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
    transition:
      background 160ms ease,
      border-color 160ms ease;
  }
  .server-choices label.checked {
    color: var(--text);
    border-color: color-mix(
      in srgb,
      var(--server-accent) 28%,
      var(--border-strong)
    );
    background: var(--surface-1);
  }
  .server-choices input {
    appearance: none;
    display: grid;
    place-content: center;
    width: 14px;
    height: 14px;
    margin: 0;
    border: 1px solid var(--border-strong);
    border-radius: 3px;
    background: transparent;
    color: var(--brand-strong);
    cursor: inherit;
  }
  .server-choices input:checked {
    border-color: var(--brand-strong);
    background: var(--brand-soft);
  }
  .server-choices input:checked::after {
    content: "";
    width: 7px;
    height: 4px;
    border-left: 1.5px solid currentColor;
    border-bottom: 1.5px solid currentColor;
    transform: translateY(-1px) rotate(-45deg);
  }
  .server-choices label:has(input:focus-visible) {
    outline: var(--focus-ring);
    outline-offset: 1px;
  }
  .server-choices label:has(input:disabled) {
    cursor: default;
  }
  .server-choices label:not(.checked):has(input:disabled) {
    opacity: 0.55;
  }
  .server-choices .server-name {
    text-align: left;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--type-xs);
    font-weight: 600;
  }
  .server-preflight {
    display: flex;
    align-items: baseline;
    justify-content: flex-end;
    gap: 2px;
    color: var(--text-muted);
    font: var(--type-xs)/1.3 var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .server-preflight span {
    color: var(--text-soft);
    font-size: 9px;
  }
  .selection-help {
    margin: 0;
    font-size: var(--type-xs);
  }
  @media (prefers-reduced-motion: reduce) {
    .server-choices label {
      transition: none;
    }
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
