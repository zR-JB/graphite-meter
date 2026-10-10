<script lang="ts">
  import { onMount } from "svelte";
  import Icon from "./Icon.svelte";
  import { authenticatedFetch } from "../auth";
  import { readJSONResponse, parseAccountSession } from "../api/decode";
  import { tooltip } from "../actions/tooltip";
  import { getApplicationController } from "../runner/controllerContext";
  import ConfirmDialog from "./ConfirmDialog.svelte";

  const app = getApplicationController();

  let session = $state.raw<ReturnType<typeof parseAccountSession> | null>(null);
  let form = $state<HTMLFormElement>();
  let everywhere = $state<HTMLButtonElement>();
  let confirming = $state(false);

  const label = $derived(
    session?.provider === "local" ? "Local operator" : (session?.name ?? ""),
  );
  const provider = $derived(
    session?.provider === "local"
      ? "Operator session"
      : (session?.provider ?? ""),
  );

  onMount(() => {
    const controller = new AbortController();
    // Not motion: the session request gives up after three seconds.
    const timeout = setTimeout(() => controller.abort(), 3000);
    void (async () => {
      try {
        const response = await authenticatedFetch("/auth/session", {
          cache: "no-store",
          signal: controller.signal,
        });
        if (response.ok) {
          const parsed = parseAccountSession(await readJSONResponse(response));
          if (!controller.signal.aborted) session = parsed;
        }
      } catch {
        // Leave the control unrendered; the runner surfaces connectivity loss.
      } finally {
        clearTimeout(timeout);
      }
    })();
    return () => {
      clearTimeout(timeout);
      controller.abort();
    };
  });
</script>

{#if session}
  <form
    bind:this={form}
    class="account"
    method="post"
    action="/auth/logout"
    aria-label={`${label}, ${provider}`}
    onsubmit={app.signOut}
  >
    <input type="hidden" name="csrf" value={session.csrf} />
    <div class="identity" {@attach tooltip(() => `${label}\n${provider}`)}>
      <span class="avatar" aria-hidden="true"><Icon name="person" /></span>
      <strong class="name">{label}</strong>
    </div>
    <button
      bind:this={everywhere}
      class="btn btn-icon key signout everywhere"
      type="submit"
      name="scope"
      value="all"
      onclick={(event) => {
        event.preventDefault();
        confirming = true;
      }}
      {@attach tooltip(() => "End all sessions for this account")}
      aria-label={`Sign out ${label} everywhere`}
    >
      <Icon name="power" />
    </button>
    <button
      class="btn btn-icon key signout"
      type="submit"
      {@attach tooltip(() => "Sign out")}
      aria-label={`Sign out ${label}`}
    >
      <Icon name="exit" />
    </button>
  </form>
  <ConfirmDialog
    open={confirming}
    id="sign-out-everywhere"
    invoker={everywhere}
    title="Sign out everywhere?"
    description={`End every session of ${label} on all devices, including this one.`}
    cancelLabel="Stay signed in"
    confirmLabel="Sign out everywhere"
    onCancel={() => (confirming = false)}
    onConfirm={() => {
      confirming = false;
      form?.requestSubmit(everywhere);
    }}
  />
{/if}

<style>
  /* Who is signed in, then the two sign-out scopes as top-bar keys like their neighbours.
     The provider lives in the tooltip and the form's accessible name. */
  .account {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    max-width: min(36vw, 300px);
  }
  .identity {
    display: flex;
    flex: 1 1 auto;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    padding: 0 var(--space-2);
  }
  .avatar {
    display: grid;
    flex: none;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--r-full);
    background: var(--track);
    color: var(--text-muted);
  }
  .avatar :global(svg) {
    width: var(--icon-sm);
    height: var(--icon-sm);
  }
  .name {
    overflow: hidden;
    min-width: 0;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-body) / 1.3 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (hover: hover) {
    .signout:hover {
      color: var(--err);
    }
  }
  /* Narrow and touch layouts keep only the sign-out action. */
  @media (max-width: 759px), (pointer: coarse) {
    .account {
      display: contents;
    }
    .identity,
    .everywhere {
      display: none;
    }
  }
</style>
