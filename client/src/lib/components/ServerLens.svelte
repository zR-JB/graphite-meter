<script lang="ts">
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
</script>

<span class="lens">
  <ServerScope
    quiet
    {servers}
    value={store.resultScope}
    onchange={controller.showServer}
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
  }
</style>
