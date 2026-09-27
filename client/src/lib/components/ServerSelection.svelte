<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import { fmtMs } from "../format";
  import ServerScope from "./ServerScope.svelte";
  import {
    serverAccent,
    catalogSelection,
  } from "../presentation/serverAppearance";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { JARGON, preflightNote, READINESS } from "../presentation/vocabulary";
  const controller = getApplicationController();
  const descriptionId = $props.id();
  let retrying = $state<string[]>([]);
  const choices = new Map<string, HTMLLabelElement>();
  async function retry(serverId: string, button: HTMLButtonElement) {
    if (locked || retrying.includes(serverId)) return;
    retrying = [...retrying, serverId];
    try {
      await controller.retry({ id: serverId });
    } finally {
      if (
        store.servers.get(serverId)?.readiness === "verified" &&
        document.activeElement === button
      ) {
        const choice = choices.get(serverId);
        (
          choice?.querySelector<HTMLInputElement>("input:not(:disabled)") ??
          choice
        )?.focus({ preventScroll: true });
      }
      retrying = retrying.filter((id) => id !== serverId);
    }
  }
  const selected = $derived(
    catalogSelection(store.serverCatalog, store.selectedServers),
  );
  const locked = $derived(store.isRunning || store.preparing);
  const problems = $derived(
    (store.serverCatalog?.servers ?? []).filter(
      (server) =>
        store.selectedServers.includes(server.id) &&
        (store.selectedServers.length > 1 ||
          store.servers.get(server.id)?.readiness === "sign-in") &&
        (retrying.includes(server.id) ||
          ["failed", "sign-in"].includes(
            store.servers.get(server.id)?.readiness ?? "unchecked",
          )),
    ),
  );
</script>

{#if (store.serverCatalog?.servers.length ?? 0) > 1}
  <div class="server-setting">
    <div class="server-heading">
      <span {@attach tooltip(() => JARGON.testServers)}>Test servers</span>
      <small>{selected.length} selected · up to 4</small>
    </div>
    <div
      class="server-choices"
      role="group"
      aria-label="Servers to test"
      aria-busy={store.serverMetadataLoading}
    >
      {#each store.serverCatalog!.servers as server (server.id)}
        {@const checked = store.selectedServers.includes(server.id)}
        {@const readiness = store.servers.get(server.id)?.readiness}
        {@const status =
          !readiness ||
          readiness === "unchecked" ||
          (readiness === "verified" && !checked)
            ? ""
            : READINESS[readiness].label}
        {@const preflightMs = store.servers.get(server.id)?.discovery
          ?.preflightMs}
        {@const unavailable =
          locked || (checked ? selected.length === 1 : selected.length >= 4)}
        <label
          tabindex="-1"
          {@attach (element) => {
            choices.set(server.id, element);
            return () => {
              choices.delete(server.id);
            };
          }}
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
            aria-describedby={preflightMs == null ? undefined : descriptionId}
            onchange={() =>
              controller.applyServers(
                checked
                  ? store.selectedServers.filter((id) => id !== server.id)
                  : [...store.selectedServers, server.id],
              )}
          />
          <span class="server-identity">
            <span
              class="server-name"
              {@attach tooltip(() =>
                [
                  server.name,
                  [new URL(server.url).host, server.location]
                    .filter(Boolean)
                    .join(" · "),
                  preflightMs == null ? "" : preflightNote(fmtMs(preflightMs)),
                ]
                  .filter(Boolean)
                  .join("\n"),
              )}>{server.name}</span
            >
            {#if server.location || status}<small
                >{server.location}{#if server.location && status}{" · "}{/if}{#if status}<span
                    class="server-status"
                    data-state={readiness}>{status}</span
                  >{/if}</small
              >{/if}
          </span>
          {#if preflightMs != null && !["failed", "sign-in", "checking"].includes(readiness ?? "")}<small
              class="server-preflight"
              ><span class="sr-only">Preflight request </span>{fmtMs(
                preflightMs,
              )}<span>ms</span></small
            >{/if}
        </label>
      {/each}
    </div>
    <span class="sr-only" id={descriptionId}
      >Preflight request times include connection setup and the response. They
      are not latency measurements.</span
    >
  </div>
{/if}
{#if store.unresolvedServers.length}
  <div class="selection-notice" role="status">
    <p>The saved selection has changed.</p>
    <button
      class="btn"
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
      class="btn"
      type="button"
      disabled={locked || store.catalogLoading}
      onclick={() => void controller.retryCatalog()}>Retry servers</button
    >
  </div>
{/if}
{#each problems as server (server.id)}
  {@const pending = retrying.includes(server.id)}
  <div class="server-feedback" role="status">
    <div class="feedback-row">
      <p class="feedback-message">
        <strong>{server.name}</strong>
        {#key pending}<span class="enter"
            >{pending
              ? `${READINESS.checking.label}…`
              : store.servers.get(server.id)?.message}</span
          >{/key}
      </p>
      {#if store.servers.get(server.id)?.readiness === "sign-in"}
        <button
          class="btn"
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
          class="btn"
          type="button"
          disabled={locked}
          aria-label={`Retry ${server.name}`}
          aria-disabled={pending}
          aria-busy={pending}
          onclick={(event) => void retry(server.id, event.currentTarget)}
          >Retry</button
        >
      {/if}
    </div>
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
      <button
        class="btn"
        type="button"
        onclick={controller.cancelServerApproval}>Cancel sign-in</button
      >
      {#if !store.serverApproval.renewUrl}
        <p>
          <a
            href={store.serverApproval.url}
            target="_blank"
            rel="noopener noreferrer">Open sign-in page</a
          >
          if the sign-in window did not open. Return here after approval. Canceling
          stops this interface's approval; you can close any remaining sign-in window
          yourself.
          {store.serverApproval.message ?? ""}
        </p>
      {/if}
    {/if}
  </div>
{/each}

{#if selected.length > 1 && store.latencyEnabled}
  <label class="latency-policy">
    <span {@attach tooltip(() => JARGON.latencyServer)}>Latency server</span>
    <ServerScope
      servers={selected}
      value={store.latencySelection.mode === "all"
        ? ""
        : store.primaryLatencyServer}
      label="Latency measurement servers"
      aggregate="Combined"
      hint="Measure latency to every server"
      disabled={locked}
      onchange={(id) =>
        controller.configureLatency(
          id ? "primary" : "all",
          id || store.primaryLatencyServer,
        )}
    />
  </label>
{/if}

<style>
  .server-setting {
    display: grid;
    gap: var(--space-2);
    min-width: 0;
  }
  .server-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }
  strong {
    font-weight: var(--w-strong);
  }
  small {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .latency-policy {
    --scope-width: 100%;
    display: grid;
    justify-items: start;
    gap: 6px;
    min-width: 0;
    padding-top: var(--space-1);
  }
  .server-choices {
    display: grid;
    gap: 2px;
    max-height: 220px;
    overflow-y: auto;
  }
  .server-choices label {
    --ring-offset: 1px;
    display: grid;
    grid-template-columns: 14px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    min-height: var(--control-h);
    padding: 6px var(--space-2);
    border: 1px solid transparent;
    border-radius: var(--r-well);
    color: var(--text-muted);
    transition: var(--transition-control);
  }
  .server-choices label.checked {
    border-color: color-mix(
      in srgb,
      var(--server-accent) 28%,
      var(--border-strong)
    );
    background: var(--surface-1);
    color: var(--text);
  }
  .server-choices input {
    appearance: none;
    display: grid;
    place-content: center;
    width: 14px;
    height: 14px;
    border: 1px solid var(--border-strong);
    border-radius: 3px;
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
  .server-choices label:has(input:disabled) {
    cursor: default;
  }
  .server-choices label:not(.checked):has(input:disabled) {
    opacity: 0.55;
  }
  .server-identity {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .server-name {
    overflow: hidden;
    font-size: var(--type-sm);
    font-weight: var(--w-strong);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .server-identity small {
    overflow: hidden;
    line-height: 1.3;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .server-status[data-state="failed"],
  .server-status[data-state="sign-in"] {
    color: var(--warn);
  }
  .server-status[data-state="verified"] {
    color: var(--brand-strong);
  }
  .server-preflight {
    display: flex;
    align-items: baseline;
    gap: 2px;
    font: var(--type-xs) / 1.3 var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .server-preflight span {
    color: var(--text-soft);
    font-size: var(--type-2xs);
  }
  .feedback-message {
    min-height: 1.5em;
  }
  .feedback-message span {
    display: inline-block;
  }
  .server-feedback,
  .selection-notice {
    display: grid;
    justify-items: start;
    gap: var(--space-1);
    padding-block: var(--space-1);
    font: var(--type-xs) / 1.5 var(--font-sans);
  }
  .feedback-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-1) var(--space-2);
    width: 100%;
  }
  p {
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .approval-code strong {
    display: block;
    padding-block: var(--space-1);
    font: var(--w-strong) var(--type-lg) / 1.4 var(--font-mono);
    letter-spacing: var(--track-wide);
  }
</style>
