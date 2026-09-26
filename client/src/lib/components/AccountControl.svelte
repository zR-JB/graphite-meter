<script lang="ts">
  import { onMount } from "svelte";
  import { authenticatedFetch } from "../auth";
  import { readJSONResponse, parseAccountSession } from "../api/decode";
  import { tooltip } from "../actions/tooltip";

  let session = $state<ReturnType<typeof parseAccountSession> | null>(null);

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
    class="account"
    method="post"
    action="/auth/logout"
    aria-label={`${label} · ${provider}`}
  >
    <input type="hidden" name="csrf" value={session.csrf} />
    <div class="identity" {@attach tooltip(() => `${label} · ${provider}`)}>
      <span class="avatar" aria-hidden="true">
        <svg viewBox="0 0 20 20">
          <circle cx="10" cy="7" r="3" />
          <path d="M4.5 16c.5-3 2.3-4.5 5.5-4.5s5 1.5 5.5 4.5" />
        </svg>
      </span>
      <strong class="name">{label}</strong>
    </div>
    <button
      class="btn btn-icon btn-quiet signout everywhere"
      type="submit"
      name="scope"
      value="all"
      {@attach tooltip(() => "End all sessions for this account")}
      aria-label={`Sign out ${label} everywhere`}
    >
      <svg viewBox="0 0 20 20" aria-hidden="true">
        <path d="M10 3v7" />
        <path d="M6 5.5a6 6 0 1 0 8 0" />
      </svg>
    </button>
    <button
      class="btn btn-icon btn-quiet signout"
      type="submit"
      {@attach tooltip(() => "Sign out")}
      aria-label={`Sign out ${label}`}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path
          d="M9 4H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h4M16 7l5 5-5 5M9 12h12"
        />
      </svg>
    </button>
  </form>
{/if}

<style>
  /* One control strip: identity, then the two sign-out scopes. The provider
     lives in the tooltip and the form's accessible name. */
  .account {
    display: flex;
    align-items: center;
    min-width: 0;
    max-width: min(36vw, 300px);
    height: var(--control-h);
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: var(--elev-tile);
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
    width: 20px;
    height: 20px;
    border: 1px solid var(--brand-line);
    border-radius: var(--r-full);
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  svg {
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.6;
  }
  .avatar svg {
    width: 13px;
  }
  .name {
    overflow: hidden;
    min-width: 0;
    font-size: var(--type-sm);
    font-weight: var(--w-strong);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .signout {
    height: 100%;
    border-radius: 0;
    box-shadow: -1px 0 0 var(--border);
  }
  @media (hover: hover) {
    .signout:hover {
      background: var(--err-soft);
      color: var(--err);
    }
  }
  .signout:focus-visible {
    outline-offset: -3px;
  }
  /* Narrow and touch layouts keep only the sign-out action, as a topbar
     button of its own. */
  @media (max-width: 759px), (pointer: coarse) {
    .account {
      display: contents;
    }
    .identity,
    .everywhere {
      display: none;
    }
    .signout {
      --btn-line: var(--border);
      height: auto;
      border-radius: calc(var(--r-chrome) + var(--hit-pad));
      background: var(--surface-2) padding-box;
      box-shadow:
        inset 0 0 0 1px var(--btn-line),
        var(--elev-tile);
    }
  }
</style>
