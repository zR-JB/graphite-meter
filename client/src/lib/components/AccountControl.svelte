<script lang="ts">
  import { onMount } from "svelte";
  import { authenticatedFetch, requireAuthentication } from "../auth";

  type Session = {
    name: string;
    provider: string;
    expires: string;
    csrf: string;
  };
  let session = $state<Session | null>(null);

  onMount(async () => {
    const response = await authenticatedFetch("/auth/session", {
      cache: "no-store",
    });
    if (response.status === 403) {
      requireAuthentication();
      return;
    }
    if (response.ok) session = (await response.json()) as Session;
  });
</script>

{#if session}
  <form
    class="account"
    method="post"
    action="/auth/logout"
    title={`${session.name} · ${session.provider}`}
  >
    <input type="hidden" name="csrf" value={session.csrf} />
    <span
      >{session.provider === "local"
        ? "Local operator"
        : `${session.name} · ${session.provider}`}</span
    >
    <button class="ghost-btn" type="submit">Sign out</button>
  </form>
{/if}

<style>
  .account {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    max-width: min(34vw, 360px);
  }
  .account span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: 12px;
  }
</style>
