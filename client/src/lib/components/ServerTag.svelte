<script lang="ts">
  import { tooltip } from "../actions/tooltip";
  import type { ServerIdentity } from "../servers/catalog";
  import { store } from "../state/store.svelte";
  import { serverAccent } from "../presentation/serverAppearance";
  let {
    servers,
    id = "",
    label = "Measurement source",
  }: {
    servers: readonly ServerIdentity[];
    id?: string;
    label?: string;
  } = $props();
  const server = $derived(servers.find((server) => server.id === id));
  const description = $derived(
    server
      ? [label, server.name, server.location, new URL(server.url).host]
          .filter(Boolean)
          .join("\n")
      : `${label}\nAggregate of ${servers.length} servers\n${servers.map((server) => [server.name, server.location].filter(Boolean).join(" — ")).join("\n")}`,
  );
</script>

<span
  class="server-tag"
  style:--server-accent={server
    ? serverAccent(server, store.serverCatalog?.servers ?? servers)
    : "var(--text-soft)"}
  use:tooltip={description}
  aria-label={description.replaceAll("\n", ", ")}
  ><span class="source-mark" aria-hidden="true"></span><span class="source-name"
    >{server ? server.name : "All servers"}</span
  ></span
>

<style>
  .server-tag {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
    color: var(--text-muted);
    font: 500 var(--type-xs)/1.4 var(--font-sans);
    white-space: nowrap;
    cursor: help;
  }
  .source-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .source-mark {
    width: 3px;
    height: 10px;
    border-radius: 1px;
    flex-shrink: 0;
    background: var(--server-accent);
  }
  .server-tag:focus-visible {
    outline: 2px solid var(--brand);
    outline-offset: 2px;
  }
</style>
