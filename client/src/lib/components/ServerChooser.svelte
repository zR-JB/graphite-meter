<script lang="ts">
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { acquirePageScrollLock } from "../actions/pageScrollLock";
  const controller = getApplicationController();
  let dialog: HTMLDialogElement;
  let draft = $state<string[]>([]);
  let error = $state("");
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
    if (draft.includes(id)) draft = draft.filter((value) => value !== id);
    else if (draft.length < 4) draft = [...draft, id];
    else error = "Select up to four servers.";
  }
  function apply() {
    try {
      if (controller.applyServers(draft)) error = "";
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "Check the selection";
    }
  }
  const labels = {
    unchecked: "Not checked",
    checking: "Checking…",
    ready: "Ready",
    "sign-in": "Sign in required",
    failed: "Unavailable",
  };
</script>

<button
  class="server-trigger"
  type="button"
  onclick={() => controller.openServers()}
  disabled={store.isRunning || store.preparing}
  aria-haspopup="dialog"
  >Servers <span>· {store.selectedServers.length} selected</span></button
>
<dialog
  bind:this={dialog}
  class="server-dialog"
  aria-labelledby="server-chooser-title"
  aria-describedby="server-chooser-description"
  oncancel={(event) => {
    event.preventDefault();
    controller.closeServers();
  }}
>
  <header>
    <h2 id="server-chooser-title">Servers</h2>
    <span>{draft.length} selected</span><button
      type="button"
      class="close"
      aria-label="Cancel server selection"
      onclick={() => controller.closeServers()}>×</button
    >
  </header>
  <p id="server-chooser-description">
    Tests combine the selected servers’ throughput. Choose one to four.
  </p>
  {#if store.unresolvedServers.length}
    <div class="notice" role="status">
      Saved servers were removed or changed. Apply a selection to continue.
      {#each store.unresolvedServers as server}<small
          >{server.id} · {server.url}</small
        >{/each}
    </div>
  {/if}
  <div class="server-list">
    {#if !store.serverCatalog}
      <p role="status">
        {store.catalogLoading
          ? "Loading servers…"
          : store.startError || "The server catalogue is unavailable."}
      </p>
      <button
        type="button"
        disabled={store.catalogLoading}
        onclick={() => void controller.retryCatalogue()}>Retry catalogue</button
      >
    {/if}
    {#each store.serverCatalog?.servers ?? [] as server (server.id)}
      {@const readiness = store.serverReadiness[server.id] ?? {
        state: "unchecked",
      }}
      <section class="server-row" class:selected={draft.includes(server.id)}>
        <div class="row-main">
          <label
            ><input
              type="checkbox"
              checked={draft.includes(server.id)}
              onchange={() => toggle(server.id)}
              disabled={!draft.includes(server.id) && draft.length >= 4}
            /><span
              ><strong>{server.name}</strong><small
                >{server.id === "self" ? "This server" : ""}{server.id ===
                  "self" && server.location
                  ? " · "
                  : ""}{server.location ?? ""}</small
              ></span
            ></label
          >
          <span
            class="readiness"
            class:ready={readiness.state === "ready"}
            role="status">{labels[readiness.state]}</span
          >
        </div>
        <details>
          <summary>Connection details</summary>
          <p>{server.url}</p>
          {#if server.additionalOrigins?.length}<p>
              Additional origins: {server.additionalOrigins.join(", ")}
            </p>{/if}{#if readiness.message}<p>{readiness.message}</p>{/if}
        </details>
        {#if readiness.state === "sign-in" || readiness.state === "failed"}
          <div class="row-actions">
            {#if readiness.state === "sign-in"}<button
                type="button"
                onclick={() => void controller.signInServer(server.id)}
                >Sign in…</button
              >{:else}<button
                type="button"
                onclick={() => void controller.retryServer(server.id)}
                >Retry</button
              >{/if}
            {#if draft.includes(server.id)}<button
                type="button"
                onclick={() => toggle(server.id)}>Remove</button
              >{/if}
          </div>
        {/if}
        {#if store.serverApproval?.id === server.id}
          <p class="approval">
            Complete approval in the other window. <a
              href={store.serverApproval.url}
              target="_blank"
              rel="noopener noreferrer">Open sign-in page</a
            >{store.serverApproval.message ?? ""}
          </p>
        {/if}
      </section>
    {/each}
  </div>
  {#if error}<p class="notice" role="alert">{error}</p>{/if}
  <footer>
    <button
      class="ghost-btn"
      type="button"
      onclick={() => controller.closeServers()}>Cancel</button
    ><button
      class="apply"
      type="button"
      disabled={draft.length < 1 || draft.length > 4}
      onclick={apply}>Apply</button
    >
  </footer>
</dialog>

<style>
  .server-trigger {
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    padding: 0.55rem 0.8rem;
    background: var(--surface-1);
    color: var(--text);
    font: inherit;
    font-size: 0.75rem;
    white-space: nowrap;
    cursor: pointer;
  }
  .server-trigger span {
    color: var(--text-muted);
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .server-dialog {
    color: var(--text);
    background: var(--surface-1);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    box-shadow: var(--shadow-float);
    width: min(520px, calc(100vw - 2rem));
    max-height: calc(100svh - 2rem);
    padding: 1.25rem;
    overscroll-behavior: contain;
    transition:
      opacity 0.16s,
      transform 0.16s;
  }
  .server-dialog::backdrop {
    background: color-mix(in srgb, var(--canvas) 70%, transparent);
    backdrop-filter: blur(3px);
  }
  header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  h2 {
    font-size: 1.15rem;
    flex: 1;
    margin: 0;
  }
  header > span {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .close {
    border: 0;
    background: transparent;
    color: var(--text-muted);
    font-size: 1.5rem;
    padding: 0.2rem 0.4rem;
    cursor: pointer;
  }
  p {
    font-size: 0.8rem;
    color: var(--text-muted);
    line-height: 1.5;
  }
  .server-list {
    display: grid;
    gap: 0.5rem;
    margin: 1rem 0;
  }
  .server-row {
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    padding: 0.75rem;
  }
  .server-row.selected {
    border-color: var(--border-strong);
    background: color-mix(in srgb, var(--text) 3%, transparent);
  }
  .row-main {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    justify-content: space-between;
  }
  label {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    cursor: pointer;
    min-width: 0;
  }
  input {
    width: 1rem;
    height: 1rem;
    accent-color: var(--brand);
  }
  strong {
    display: block;
    font-size: 0.875rem;
    font-weight: 550;
    overflow-wrap: anywhere;
  }
  small {
    display: block;
    font-size: 0.7rem;
    color: var(--text-muted);
    margin-top: 0.2rem;
  }
  .readiness {
    font-size: 0.7rem;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .readiness.ready {
    color: var(--brand);
  }
  details {
    font-size: 0.7rem;
    color: var(--text-muted);
    margin: 0.55rem 0 0 1.75rem;
  }
  summary {
    cursor: pointer;
  }
  details p {
    font-family: var(--font-mono);
    font-size: 0.7rem;
    overflow-wrap: anywhere;
  }
  .row-actions {
    display: flex;
    gap: 0.9rem;
    margin: 0.6rem 0 0 1.75rem;
  }
  .row-actions button {
    border: 0;
    background: none;
    font: inherit;
    font-size: 0.75rem;
    color: var(--text);
    padding: 0.15rem 0;
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 0.2rem;
  }
  .notice {
    padding: 0.75rem;
    border-left: 2px solid var(--phase-upload);
    background: var(--surface-2);
    font-size: 0.8rem;
  }
  .approval {
    margin-bottom: 0;
  }
  a {
    color: var(--text);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 0.75rem;
    margin-top: 1rem;
  }
  .apply {
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--text);
    color: var(--canvas);
    padding: 0.55rem 1.4rem;
    font: inherit;
    font-size: 0.8rem;
    cursor: pointer;
  }
  button:focus-visible,
  summary:focus-visible,
  a:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--brand);
    outline-offset: 3px;
  }
  @starting-style {
    .server-dialog[open] {
      opacity: 0;
      transform: translateY(6px);
    }
  }
  @media (max-width: 600px) {
    .server-dialog {
      margin: auto 0 0;
      width: 100%;
      max-width: none;
      max-height: 85svh;
      border-radius: var(--r-chrome) var(--r-chrome) 0 0;
      padding-bottom: max(1.25rem, env(safe-area-inset-bottom));
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .server-dialog {
      transition: none;
    }
  }
</style>
