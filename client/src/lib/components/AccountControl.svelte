<script lang="ts">
  import { onMount } from "svelte";
  import { authenticatedFetch } from "../auth";
  import { tooltip } from "../actions/tooltip";

  type Session = {
    name: string;
    provider: string;
    expires: string;
    csrf: string;
  };
  let session = $state<Session | null>(null);

  const label = $derived(
    session?.provider === "local" ? "Local operator" : (session?.name ?? ""),
  );
  const provider = $derived(
    session?.provider === "local"
      ? "Operator session"
      : (session?.provider ?? ""),
  );

  // authenticatedFetch redirects only on a 403 carrying the
  // Graphite-Meter-Auth marker. A bare 403 from a proxy or WAF is not an
  // expired session: treating it as one loops through /login.
  onMount(async () => {
    try {
      const response = await authenticatedFetch("/auth/session", {
        cache: "no-store",
      });
      if (response.ok) session = (await response.json()) as Session;
    } catch {
      // Leave the control unrendered; the runner surfaces connectivity loss.
    }
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
    <div class="identity" use:tooltip={`${label} · ${provider}`}>
      <span class="avatar" aria-hidden="true">
        <svg viewBox="0 0 20 20">
          <circle cx="10" cy="7" r="3" />
          <path d="M4.5 16c.5-3 2.3-4.5 5.5-4.5s5 1.5 5.5 4.5" />
        </svg>
      </span>
      <strong class="name">{label}</strong>
    </div>
    <button
      class="signout everywhere"
      type="submit"
      name="scope"
      value="all"
      use:tooltip={"Sign out everywhere — end every session for this account, including ones you can no longer reach"}
      aria-label={`Sign out ${label} everywhere`}
    >
      <svg viewBox="0 0 20 20" aria-hidden="true">
        <path d="M10 3v7" />
        <path d="M6 5.5a6 6 0 1 0 8 0" />
      </svg>
    </button>
    <button
      class="signout"
      type="submit"
      use:tooltip={"Sign out"}
      aria-label={`Sign out ${label}`}
    >
      <svg viewBox="0 0 20 20" aria-hidden="true">
        <path
          d="M8 4H5.5A1.5 1.5 0 0 0 4 5.5v9A1.5 1.5 0 0 0 5.5 16H8M12.5 6.5 16 10l-3.5 3.5M8 10h8"
        />
      </svg>
    </button>
  </form>
{/if}

<style>
  .account {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    min-width: 0;
    height: 32px;
    max-width: min(36vw, 300px);
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: inset 0 1px 0 var(--edge-light);
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
    width: 20px;
    height: 20px;
    place-items: center;
    border: 1px solid color-mix(in srgb, var(--brand) 38%, var(--border));
    border-radius: var(--r-full);
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  .avatar svg {
    width: 13px;
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.5;
  }
  /* The strip carries one 32px line. The provider lives in the tooltip and
     the form's accessible name. */
  .name {
    overflow: hidden;
    min-width: 0;
    color: var(--text);
    font-size: var(--type-sm);
    font-weight: 650;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .signout {
    display: grid;
    flex: none;
    width: 32px;
    height: 100%;
    place-items: center;
    border: 0;
    border-left: 1px solid var(--border);
    background: transparent;
    color: var(--text-muted);
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .signout:hover {
    background: var(--err-soft);
    color: var(--err);
  }
  .signout:focus-visible {
    outline: var(--focus-ring);
    outline-offset: -3px;
  }
  .signout svg {
    width: 16px;
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.6;
  }
  @media (max-width: 640px) {
    .name {
      display: none;
    }
  }
  @media (max-width: 430px), (pointer: coarse) and (max-width: 759px) {
    .identity,
    .everywhere {
      display: none;
    }
    .account {
      width: 32px;
    }
    .signout {
      border-left: 0;
    }
  }
</style>
