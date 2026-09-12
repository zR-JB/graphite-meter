<script lang="ts">
  import type { MultiServerResult } from "../servers/measurement";
  import ServerSelector from "./ServerSelector.svelte";
  import DiagnosticDetails from "./DiagnosticDetails.svelte";
  let {
    details,
    value,
    onchange,
  }: {
    details: MultiServerResult;
    value: string;
    onchange: (id: string) => void;
  } = $props();
  const server = $derived(
    details.selection.find((server) => server.id === value),
  );
  const failures = $derived(
    details.failures.filter(
      (failure) => !server || failure.serverId === server.id,
    ),
  );
</script>

<div class="result-server-context">
  <div class="scope-heading">
    {#if details.participants.length < details.selection.length}<span
        class="scope-caption"
        >{details.participants.length} of {details.selection.length} servers</span
      >{/if}
    <ServerSelector
      servers={details.selection}
      {value}
      {onchange}
      aggregate
      aggregateLabel="Combined"
      label="Result measurements"
    />
    {#if failures.length}<DiagnosticDetails
        label={`${failures.length} ${failures.length === 1 ? "issue" : "issues"}`}
      >
        <ul class="issues">
          {#each failures as failure}<li>
              <strong
                >{details.selection.find(
                  (server) => server.id === failure.serverId,
                )?.name} · {failure.stage}{failure.scope === "latency"
                  ? " latency"
                  : ""}</strong
              >
              <p>{failure.message}</p>
            </li>{/each}
        </ul>
      </DiagnosticDetails>{/if}
  </div>
</div>

<style>
  .result-server-context {
    min-width: 0;
    display: grid;
    gap: var(--space-2);
    color: var(--text-soft);
    font: var(--type-sm)/1.4 var(--font-sans);
  }
  .scope-heading {
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
  }
  .scope-caption {
    color: var(--text-muted);
    font-size: var(--type-xs);
    font-weight: 500;
  }
  .issues {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: var(--space-3);
    font-size: var(--type-xs);
    color: var(--text-muted);
  }
  .issues strong {
    color: var(--text);
  }
  .issues p {
    margin: 4px 0 0;
  }
</style>
