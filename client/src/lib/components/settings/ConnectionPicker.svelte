<script lang="ts">
  import { store } from "../../state/store.svelte";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  import { summarizeRoleValidation } from "../../runner/paths";
  import type { ConnectionRole } from "../../runner/contract";
  import type { PathOption } from "../../presentation/paths";
  import {
    IN_USE,
    JARGON,
    PATH_NOTE,
    READINESS,
  } from "../../presentation/vocabulary";
  import { tooltip } from "../../actions/tooltip";
  import { reveal } from "../../presentation/motion.svelte";

  interface Props {
    role: ConnectionRole;
    options: readonly PathOption[];
    locked?: boolean;
  }
  let { role, options, locked = false }: Props = $props();
  const labelId = $props.id();

  const selected = $derived(
    role === "throughput"
      ? store.config.transports.throughputTarget
      : store.config.transports.latencyTarget,
  );
  const connection = $derived(store.connections[role]);
  const simultaneous = $derived(store.selectedServers.length > 1);
  // The server list states a server's own failure once; the paths speak for the rest.
  const unlisted = $derived(
    store.selectedServers.filter((id) => {
      const readiness = store.servers.get(id)?.readiness;
      return !(
        readiness === "sign-in" ||
        (readiness === "failed" && store.selectedServers.length > 1)
      );
    }),
  );
  const roleSummary = $derived(
    summarizeRoleValidation(role, unlisted, store.servers),
  );
  const validation = $derived(
    simultaneous ? roleSummary.state : connection.validation,
  );
  const summary = $derived(
    !simultaneous
      ? (connection.message ?? connection.summary)
      : roleSummary.verified === roleSummary.total
        ? "Resolved per server"
        : `${roleSummary.verified} of ${roleSummary.total} servers ready`,
  );
  // A failure's message names it, so it stands in for the status word.
  const failure = $derived(
    !simultaneous && validation === "failed" ? connection.message : undefined,
  );
  const status = $derived(
    locked
      ? IN_USE
      : failure
        ? { ...READINESS.failed, label: failure }
        : READINESS[validation],
  );
  const title = $derived(
    role === "throughput" ? "Throughput path" : "Latency path",
  );
  // A forced choice a selected server lacks fails that server, which the list above names.
  const offerAutomatic = $derived(
    selected !== "auto" &&
      !locked &&
      (validation === "failed" ||
        !!options.find((option) => option.value === selected)?.disabled ||
        store.selectedServers.some(
          (id) => store.servers.get(id)?.readiness === "failed",
        )),
  );
  // Unavailable choices fold into one line after the available ones; the selected one always shows.
  let unfolded = $state(false);
  const folded = $derived(
    options.filter((option) => option.disabled && option.value !== selected),
  );
  const available = $derived(
    options.filter((option) => !folded.includes(option)),
  );
  function select(value: string) {
    controller.selectConnection(role, value);
  }
</script>

{#snippet choice(option: PathOption)}
  <input
    class="check"
    type="radio"
    name={`${role}-target`}
    value={option.value}
    checked={selected === option.value}
    disabled={option.disabled || locked}
    onchange={() => select(option.value)}
  />
  <span class="choice-label"
    >{option.label}
    {#if option.disabled || PATH_NOTE[role][option.group ?? option.value]}<small
        >{option.disabled
          ? option.detail
          : PATH_NOTE[role][option.group ?? option.value]}</small
      >{/if}</span
  >
{/snippet}

<div class="picker" role="group" aria-labelledby={labelId}>
  <div class="list-label">
    <span id={labelId} {@attach tooltip(() => JARGON[`${role}Path`])}
      >{title}</span
    >
  </div>
  <div class="kv choices">
    <!-- The list follows the servers and their checks, so a swapped set lands at once: rows that unfolded and
         folded at the same time shrank the sheet under the pointer and grew it back, and the page bobbed. Only the
         unavailable rows a person unfolds reveal themselves. -->
    {#each available as option (option.value)}
      <label
        class:unavailable={option.disabled ||
          (locked && option.value !== selected)}
      >
        {@render choice(option)}
      </label>
    {/each}
    {#if unfolded}
      {#each folded as option (option.value)}
        <label transition:reveal class="unavailable">
          {@render choice(option)}
        </label>
      {/each}
    {/if}
    {#if folded.length}
      <button
        class="fold"
        type="button"
        aria-expanded={unfolded}
        {@attach tooltip(() =>
          [
            "Unavailable paths",
            ...folded.map((option) => `${option.label}: ${option.detail}`),
          ].join("\n"),
        )}
        onclick={() => (unfolded = !unfolded)}
        >{unfolded
          ? "Hide unavailable"
          : `${folded.length} more unavailable`}</button
      >
    {/if}
  </div>
  <!-- Until the server list loads there is nothing to check or retry. The line keeps a control's height whether or
       not a check leaves it a button, so a re-check never moves the rows below it. -->
  {#if store.serverCatalog}<div class="validation">
      {#if unlisted.length}
        <p>
          <span class="status-dot inline" data-tone={status.tone}></span>
          <strong>{status.label}</strong>
          {#if !failure}{summary}{/if}
        </p>
      {/if}
      {#if offerAutomatic}
        <button class="btn" type="button" onclick={() => select("auto")}
          >Use Automatic</button
        >
      {/if}
      {#if unlisted.length && !locked && (validation === "failed" || validation === "stale")}
        <!-- Both pickers mount at once and a group's name does not name a descendant
           button, so without this the rotor reads "Retry, Retry". -->
        <button
          class="btn"
          type="button"
          aria-label={`Retry ${title}`}
          onclick={() => void controller.retry({ role })}>Retry</button
        >
      {/if}
    </div>{/if}
</div>

<style>
  .picker {
    display: grid;
    gap: 6px;
    min-width: 0;
    margin-top: var(--space-3);
  }
  /* A narrow sheet wraps a choice's name and note rather than cutting them. */
  .choice-label {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .unavailable {
    cursor: not-allowed;
  }
  .unavailable > * {
    opacity: 0.5;
  }
  .fold {
    justify-content: start;
    padding-inline-start: calc(
      var(--row-inset) - 2px + var(--check) + var(--space-3)
    );
    color: var(--text-soft);
    font: var(--role-caption);
  }
  @media (hover: hover) {
    .fold:hover {
      color: var(--text);
    }
  }
  .validation {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--control-h);
    padding-inline: var(--row-inset);
    font: var(--role-caption);
  }
  .validation p {
    flex: 1;
    min-width: 0;
    color: var(--text-soft);
  }
  .validation strong {
    color: var(--text);
    font-weight: var(--w-strong);
  }
  .validation .status-dot {
    margin-inline-end: 4px;
  }
</style>
