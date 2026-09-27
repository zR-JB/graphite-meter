<script lang="ts">
  import Icon from "./Icon.svelte";
  import ServerScope from "./ServerScope.svelte";
  import type { ServerIdentity } from "../servers/catalog";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";

  let {
    servers,
    participants,
  }: {
    servers: readonly ServerIdentity[];
    participants: readonly ServerIdentity[];
  } = $props();
  const controller = getApplicationController();
  const details = $derived(store.result?.multiServer ?? store.serverDetails);
  // A server without a result of its own cannot be shown on its own.
  const unmeasured = $derived(
    details
      ? servers
          .filter(({ id }) => !details.servers.some((s) => s.server.id === id))
          .map(({ id }) => id)
      : [],
  );

  // One lens for the instrument: the cards follow it, and so does latency when that server measured it.
  function choose(id: string) {
    store.resultScope = id;
    const measured = (server: string) =>
      details?.servers.some((s) => s.server.id === server && s.latencyTarget);
    controller.focusServer(
      id && measured(id)
        ? id
        : (details?.latencyFocus ?? store.primaryLatencyServer),
    );
  }
</script>

<span class="lens">
  <Icon name="server" />
  <ServerScope
    {servers}
    value={store.resultScope}
    onchange={choose}
    label="Servers shown in the results"
    aggregate={participants.length < servers.length
      ? `${participants.length} of ${servers.length} servers`
      : `All ${servers.length} servers`}
    disabledIds={unmeasured}
  />
</span>

<style>
  .lens {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-soft);
  }
  .lens > :global(svg) {
    width: 14px;
    height: 14px;
  }
  .lens :global(.server-scope) {
    min-height: 28px;
    padding-block: 0;
    border-color: transparent;
    background-color: transparent;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-body) / 1 var(--font-sans);
  }
  @media (hover: hover) {
    .lens :global(.server-scope:hover:not(:disabled)) {
      background-color: var(--hover-wash);
    }
  }
</style>
