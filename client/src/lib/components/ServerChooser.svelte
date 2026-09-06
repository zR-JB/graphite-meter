<script lang="ts">
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { acquirePageScrollLock } from "../actions/pageScrollLock";
  const controller = getApplicationController();
  let dialog: HTMLDialogElement;
  let draft = $state<string[]>([]);
  let error = $state("");
  const labels = {
    unchecked: "Not checked",
    checking: "Checking…",
    ready: "Ready",
    "sign-in": "Sign in required",
    failed: "Unavailable",
  };
  $effect(() => {
    if (!dialog) return;
    if (store.serverChooserOpen && !dialog.open) {
      draft = [...store.selectedServers];
      error = "";
      dialog.showModal();
    }
    if (!store.serverChooserOpen && dialog.open) dialog.close();
  });
  $effect(() => {
    if (store.serverChooserOpen) return acquirePageScrollLock();
  });
  function toggle(id: string) {
    error = "";
    if (draft.includes(id)) draft = draft.filter((value) => value !== id);
    else if (draft.length < 4) draft = [...draft, id];
  }
  function apply() {
    try {
      controller.applyServers(draft);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "Check the selection";
    }
  }
</script>

<dialog
  bind:this={dialog}
  class="server-dialog"
  aria-modal="true"
  onkeydown={(event) => event.stopPropagation()}
  aria-labelledby="server-chooser-title"
  aria-describedby="server-chooser-description"
  oncancel={(event) => {
    event.preventDefault();
    controller.closeServers();
  }}
>
  <header>
    <div>
      <h2 id="server-chooser-title">Choose servers</h2>
      <p id="server-chooser-description">Select up to four to test together.</p>
    </div>
    <button
      type="button"
      class="close"
      aria-label="Cancel server selection"
      onclick={controller.closeServers}>×</button
    >
  </header>
  <div class="server-list">
    {#if store.unresolvedServers.length}
      <p class="notice" role="status">
        Some saved servers have changed. Choose your servers and apply to
        continue.
      </p>
    {/if}
    {#if !store.serverCatalog}
      <p role="status">
        {store.catalogLoading
          ? "Loading servers…"
          : store.startError || "Could not load servers."}
      </p>
      <button
        type="button"
        disabled={store.catalogLoading}
        onclick={() => void controller.retryCatalogue()}>Retry</button
      >
    {/if}
    {#each store.serverCatalog?.servers ?? [] as server (server.id)}
      {@const readiness = store.serverReadiness[server.id] ?? {
        state: "unchecked",
      }}
      <div class="server-row">
        <label>
          <input
            type="checkbox"
            checked={draft.includes(server.id)}
            onchange={() => toggle(server.id)}
            disabled={!draft.includes(server.id) && draft.length >= 4}
          />
          <span class="server-copy"
            ><strong>{server.name}</strong><small
              >{[
                server.location,
                server.id === "self" ? "This server" : new URL(server.url).host,
              ]
                .filter(Boolean)
                .join(" · ")}</small
            ></span
          >
          <span class="readiness" data-state={readiness.state}
            >{labels[readiness.state]}</span
          >
        </label>
        {#if readiness.state === "sign-in" || readiness.state === "failed"}
          <div class="row-feedback">
            <p role="status">
              {readiness.message ||
                (readiness.state === "sign-in"
                  ? "Sign in to use this server."
                  : "Could not connect to this server.")}
            </p>
            {#if readiness.state === "sign-in"}<button
                type="button"
                onclick={() => void controller.signInServer(server.id)}
                >Sign in…</button
              >
            {:else}<button
                type="button"
                onclick={() => void controller.retryServer(server.id)}
                >Retry</button
              >{/if}
          </div>
        {/if}
        {#if store.serverApproval?.id === server.id}
          <p class="approval" role="status">
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
  </div>
  <footer>
    <p role="status">
      {error ||
        (draft.length === 0
          ? "Select at least one server."
          : `${draft.length} of 4 selected`)}
    </p>
    <div>
      <button type="button" onclick={controller.closeServers}>Cancel</button
      ><button
        class="apply"
        type="button"
        disabled={draft.length < 1 || draft.length > 4}
        onclick={apply}>Apply</button
      >
    </div>
  </footer>
</dialog>

<style>
  .server-dialog {
    margin: auto;
    width: min(560px, calc(100vw - 32px));
    max-width: none;
    max-height: calc(100svh - 32px);
    padding: 0;
    color: var(--text);
    background: var(--surface-1);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-pill);
    box-shadow: 0 16px 64px #0005;
    font: 400 var(--type-md)/1.5 var(--font-sans);
    overflow: hidden;
  }
  .server-dialog[open] {
    display: flex;
    flex-direction: column;
  }
  .server-dialog::backdrop {
    background: #0008;
    backdrop-filter: blur(3px);
  }
  header,
  footer {
    display: flex;
    gap: var(--space-4);
    align-items: center;
    justify-content: space-between;
    padding: var(--space-5);
    flex: none;
  }
  header {
    align-items: flex-start;
  }
  h2 {
    margin: 0;
    font: 600 var(--type-lg)/1.3 var(--font-sans);
    letter-spacing: var(--track-tight);
  }
  p {
    margin: 6px 0 0;
    color: var(--text-muted);
    font-size: var(--type-sm);
  }
  button {
    min-height: 40px;
    padding: 8px 16px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: transparent;
    color: var(--text);
    font: 600 var(--type-sm)/1.4 var(--font-sans);
    cursor: pointer;
  }
  button:hover:not(:disabled) {
    background: var(--surface-2);
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .close {
    flex: none;
    padding: 0;
    width: 36px;
    min-height: 36px;
    border: 0;
    font-size: 24px;
    color: var(--text-muted);
    margin: -6px -8px 0 0;
  }
  .server-list {
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 0 var(--space-5);
    min-height: 0;
  }
  .server-row {
    border-top: 1px solid var(--border);
  }
  .server-row:last-child {
    border-bottom: 1px solid var(--border);
  }
  label {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) auto;
    gap: var(--space-3);
    align-items: center;
    min-height: 76px;
    padding: var(--space-3) 0;
    cursor: pointer;
  }
  input {
    width: 18px;
    height: 18px;
    accent-color: var(--brand);
  }
  input:disabled {
    opacity: 0.4;
  }
  .server-copy {
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  strong {
    font-weight: 600;
    font-size: var(--type-md);
    overflow-wrap: anywhere;
  }
  small {
    color: var(--text-muted);
    font-size: var(--type-sm);
    overflow-wrap: anywhere;
  }
  .readiness {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .readiness[data-state="ready"] {
    color: var(--ok);
  }
  .readiness[data-state="failed"],
  .readiness[data-state="sign-in"] {
    color: var(--warn);
  }
  .row-feedback {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin: 0 0 var(--space-3) 30px;
  }
  .row-feedback p {
    margin: 0;
    flex: 1;
    overflow-wrap: anywhere;
  }
  .row-feedback button {
    flex: none;
  }
  .approval,
  .notice {
    margin: 0 0 var(--space-3);
    padding: var(--space-3);
    background: var(--surface-2);
    border-radius: var(--r-well);
    overflow-wrap: anywhere;
  }
  a {
    color: var(--brand-strong);
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  footer p {
    margin: 0;
  }
  footer > div {
    display: flex;
    gap: var(--space-2);
  }
  .apply {
    background: var(--brand);
    border-color: var(--brand);
    color: var(--text-inverse);
  }
  .apply:hover:not(:disabled) {
    background: var(--brand-strong);
  }
  @media (prefers-reduced-motion: no-preference) {
    .server-dialog {
      transition:
        opacity var(--dur-slide) var(--ease-out),
        transform var(--dur-slide) var(--ease-out);
    }
    @starting-style {
      .server-dialog[open] {
        opacity: 0;
        transform: translateY(6px);
      }
    }
  }
  @media (max-width: 480px) {
    .server-dialog {
      margin: auto 0 0;
      width: 100%;
      max-height: calc(100svh - 24px);
      border-radius: var(--r-pill) var(--r-pill) 0 0;
    }
    header,
    footer {
      padding: var(--space-4);
    }
    .server-list {
      padding: 0 var(--space-4);
    }
    label {
      grid-template-columns: 18px minmax(0, 1fr);
      gap: 4px var(--space-3);
    }
    input {
      grid-row: span 2;
    }
    .readiness {
      grid-column: 2;
    }
    footer {
      padding-bottom: max(var(--space-4), env(safe-area-inset-bottom));
      flex-wrap: wrap;
    }
    footer > div {
      margin-left: auto;
    }
  }
</style>
