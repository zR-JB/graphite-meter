<script lang="ts">
  import { store } from "../../state/store.svelte";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  import { summarizeRoleValidation } from "../../runner/paths";
  import type { ConnectionRole } from "../../runner/contract";
  import type { PathOption } from "../../presentation/paths";
  import { JARGON, PATH_NOTE, READINESS } from "../../presentation/vocabulary";
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
        ? "Paths resolve independently on each server."
        : `${roleSummary.verified} of ${roleSummary.total} servers ready. Paths resolve independently.`,
  );
  const title = $derived(
    role === "throughput" ? "Throughput path" : "Latency path",
  );
  const offerAutomatic = $derived(
    selected !== "auto" &&
      !locked &&
      (validation === "failed" ||
        !!options.find((option) => option.value === selected)?.disabled),
  );
  // Unavailable choices fold into one line; the selected one always shows.
  let unfolded = $state(false);
  const folded = $derived(
    options.filter((option) => option.disabled && option.value !== selected),
  );
  const shown = $derived(
    unfolded ? options : options.filter((option) => !folded.includes(option)),
  );
  function select(value: string) {
    controller.selectConnection(role, value);
  }
</script>

<div class="picker" role="group" aria-labelledby={labelId}>
  <span
    class="list-label"
    id={labelId}
    {@attach tooltip(() => JARGON[`${role}Path`])}>{title}</span
  >
  <div class="kv choices">
    {#each shown as option (option.value)}
      <label
        transition:reveal
        class:unavailable={option.disabled || locked}
        {@attach tooltip(() => `${option.label}\n${option.detail}`)}
      >
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
      </label>
    {/each}
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
  {#if unlisted.length || offerAutomatic}<div class="validation">
      {#if unlisted.length}
        <p>
          <span class="status-dot inline" data-tone={READINESS[validation].tone}
          ></span>
          <strong>{locked ? "In use" : READINESS[validation].label}</strong>
          {summary}
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
  .choice-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
    min-height: 24px;
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
