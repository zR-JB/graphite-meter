<script lang="ts">
  import { onMount } from "svelte";
  import { authenticatedFetch } from "../auth";

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

  // authenticatedFetch already redirects on a 403 that carries the
  // Graphite-Meter-Auth marker. An unqualified 403 — a proxy or WAF refusing
  // the request — must not be read as an expired session, or a misconfigured
  // hop in front of the server turns into a /login redirect loop.
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
    <div class="identity" title={`${label} · ${provider}`}>
      <span class="avatar" aria-hidden="true">
        <svg viewBox="0 0 20 20">
          <circle cx="10" cy="7" r="3" />
          <path d="M4.5 16c.5-3 2.3-4.5 5.5-4.5s5 1.5 5.5 4.5" />
        </svg>
      </span>
      <span class="copy">
        <strong>{label}</strong>
        <small>{provider}</small>
      </span>
    </div>
    <button
      class="signout"
      type="submit"
      title="Sign out"
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
    height: 38px;
    max-width: min(36vw, 300px);
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: inset 0 1px 0 var(--edge-light);
  }
  .identity {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    padding: 0 10px;
  }
  .avatar {
    display: grid;
    flex: none;
    width: 24px;
    height: 24px;
    place-items: center;
    border: 1px solid color-mix(in srgb, var(--brand) 38%, var(--border));
    border-radius: 50%;
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  .avatar svg {
    width: 15px;
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.5;
  }
  .copy {
    display: grid;
    min-width: 0;
    line-height: 1.15;
  }
  .copy strong,
  .copy small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .copy strong {
    color: var(--text);
    font-size: var(--type-sm);
    font-weight: 650;
  }
  .copy small {
    color: var(--text-muted);
    font-size: 9px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .signout {
    display: grid;
    flex: none;
    width: 36px;
    height: 100%;
    margin-left: auto;
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
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: -3px;
  }
  .signout svg {
    width: 17px;
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.6;
  }
  @media (max-width: 640px) {
    .identity {
      padding: 0 7px;
    }
    .copy {
      display: none;
    }
  }
</style>
