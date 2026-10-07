<script lang="ts">
  import { clipTip, tooltip } from "../actions/tooltip";
  import { fmtMs } from "../format";
  import {
    serverLabel,
    catalogSelection,
  } from "../presentation/serverAppearance";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { JARGON, preflightNote, READINESS } from "../presentation/vocabulary";
  const controller = getApplicationController();
  const descriptionId = $props.id();
  const labelId = `${descriptionId}-label`;
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
  <div class="servers">
    <div class="list-label">
      <span id={labelId} {@attach tooltip(() => JARGON.testServers)}
        >Test servers</span
      >
      <small>{selected.length} selected, up to 4</small>
    </div>
    <div class="kv">
      <div
        class="choices"
        role="group"
        aria-labelledby={labelId}
        aria-busy={store.serverMetadataLoading}
      >
        {#each store.serverCatalog!.servers as server (server.id)}
          {@const checked = store.selectedServers.includes(server.id)}
          {@const readiness = store.servers.get(server.id)?.readiness}
          {@const shown =
            !readiness ||
            readiness === "unchecked" ||
            (readiness === "verified" && !checked)
              ? null
              : READINESS[readiness]}
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
            {@attach tooltip(() =>
              [
                server.name,
                new URL(server.url).host,
                server.location,
                preflightMs == null ? "" : preflightNote(fmtMs(preflightMs)),
              ]
                .filter(Boolean)
                .join("\n"),
            )}
          >
            <input
              class="check"
              type="checkbox"
              {checked}
              disabled={unavailable}
              aria-describedby={preflightMs == null ? undefined : descriptionId}
              onchange={() =>
                controller.applyServers(
                  checked
                    ? store.selectedServers.filter((id) => id !== server.id)
                    : [...store.selectedServers, server.id],
                )}
            />
            <span class="server-name" use:clipTip
              >{server.name}{#if serverLabel(server) !== server.name}
                <small>{server.location}</small>{/if}</span
            >
            {#if shown}<small class="server-status" data-state={readiness}
                ><span class="status-dot inline" data-tone={shown.tone}
                ></span>{shown.label}</small
              >{/if}
            {#if preflightMs != null && !["failed", "sign-in", "checking"].includes(readiness ?? "")}<small
                class="server-preflight"
                ><span class="sr-only">Preflight request </span>{fmtMs(
                  preflightMs,
                )}<span class="unit">ms</span></small
              >{/if}
          </label>
        {/each}
      </div>
    </div>
    <span class="sr-only" id={descriptionId}
      >Preflight request times include connection setup and the response. They
      are not latency measurements.</span
    >
  </div>
{/if}
{#if store.unresolvedServers.length}
  <div class="selection-notice" role="status">
    <p>The saved selection has changed</p>
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
      {store.catalogLoading ? "Loading servers…" : "Could not load servers"}
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
    <div>
      <strong>{server.name}</strong>{#if server.location}<small
          >{server.location}</small
        >{/if}
    </div>
    <p class="feedback-message">
      {#key pending}<span class="enter"
          >{pending
            ? `Checking ${server.name}…`
            : store.servers.get(server.id)?.readiness === "sign-in"
              ? "Admits signed-in clients only"
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
          : "Sign in"}</button
      >
    {:else}
      <button
        class="btn"
        type="button"
        disabled={locked}
        aria-disabled={pending}
        aria-busy={pending}
        aria-label={`Retry ${server.name}`}
        onclick={(event) => void retry(server.id, event.currentTarget)}
        >{pending ? READINESS.checking.label : "Retry"}</button
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
          if no window opened, then return here.
          {store.serverApproval.message ?? ""}
        </p>
      {/if}
    {/if}
  </div>
{/each}

<style>
  .servers {
    display: grid;
    gap: 6px;
  }
  .list-label {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-3);
    padding-inline: var(--row-inset);
  }
  :is(.list-label, .server-feedback) small,
  .server-status {
    color: var(--text-soft);
    font: var(--role-caption);
  }
  .choices label {
    --ring-offset: -2px;
    display: grid;
    grid-template-columns: var(--check) minmax(0, 1fr) auto 7ch;
  }
  .choices label:not(.checked):has(input:disabled) {
    opacity: 0.5;
  }
  /* Name over its place, like every two-line choice. */
  .server-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .server-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }
  .server-preflight {
    grid-column: 4;
    justify-self: end;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .server-preflight .unit {
    margin-inline-start: 2px;
    color: var(--text-soft);
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
    padding-inline: var(--row-inset);
    font: var(--role-caption);
    text-wrap: pretty;
  }
  .server-feedback {
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: var(--space-3);
  }
  .server-feedback > * {
    grid-column: 1 / -1;
  }
  .server-feedback > div,
  .feedback-message {
    grid-column: 1;
  }
  .feedback-message + .btn {
    grid-column: 2;
    grid-row: 1 / span 2;
    align-self: center;
  }
  .server-feedback > div {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
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
