<script lang="ts">
  import { store } from "../../state/store.svelte";
  import { getApplicationController } from "../../runner/controllerContext";
  const controller = getApplicationController();
  import { summarizeRoleValidation } from "../../runner/paths";
  import type { ConnectionRole } from "../../runner/contract";
  import type { PathOption } from "../../presentation/paths";
  import { JARGON, READINESS } from "../../presentation/vocabulary";
  import { tooltip } from "../../actions/tooltip";

  interface Props {
    role: ConnectionRole;
    options: readonly PathOption[];
    locked?: boolean;
  }
  let { role, options, locked = false }: Props = $props();

  const selected = $derived(
    role === "throughput"
      ? store.config.transports.throughputTarget
      : store.config.transports.latencyTarget,
  );
  const connection = $derived(store.connections[role]);
  const serverIds = $derived(
    role === "latency" && store.latencySelection.mode === "primary"
      ? [store.primaryLatencyServer]
      : store.selectedServers,
  );
  const simultaneous = $derived(serverIds.length > 1);
  // The server list states a server's own failure once; the paths speak for the rest.
  const unlisted = $derived(
    serverIds.filter((id) => {
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

<fieldset>
  <legend {@attach tooltip(() => JARGON[`${role}Path`])}>
    {title}
  </legend>
  <div class="options">
    {#each shown as option (option.value)}
      <label class="choice tile" class:unavailable={option.disabled || locked}>
        <input
          type="radio"
          name={`${role}-target`}
          value={option.value}
          checked={selected === option.value}
          disabled={option.disabled || locked}
          onchange={() => select(option.value)}
        />
        <span class="radio-dot" aria-hidden="true"></span>
        <span class="copy">
          <strong>{option.label}</strong>
          <small>{option.detail}</small>
        </span>
      </label>
    {/each}
  </div>
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
  {#if offerAutomatic}
    <button class="btn" type="button" onclick={() => select("auto")}
      >Use Automatic</button
    >
  {/if}
  {#if unlisted.length}<div class="validation">
      <span class="status-dot" data-tone={READINESS[validation].tone}></span>
      <span class="validation-copy">
        <strong>{locked ? "In use" : READINESS[validation].label}</strong>
        <small>{summary}</small>
      </span>
      {#if !locked && (validation === "failed" || validation === "stale")}
        <!-- Both pickers mount at once and a <legend> does not name a descendant
           button, so without this the rotor reads "Retry, Retry". -->
        <button
          class="btn"
          type="button"
          aria-label={`Retry ${title}`}
          onclick={() => void controller.retry({ role })}>Retry</button
        >
      {/if}
    </div>{/if}
</fieldset>

<style>
  fieldset {
    display: grid;
    gap: 6px;
    min-width: 0;
    margin-top: 6px;
  }
  legend {
    margin-bottom: 6px;
    padding-inline: var(--space-3);
    color: var(--text-soft);
    font-size: var(--type-body);
  }
  .options {
    display: grid;
    gap: 6px;
  }
  .choice {
    --ring-offset: 1px;
    position: relative;
    display: grid;
    grid-template-columns: 14px minmax(0, 1fr);
    align-items: center;
    gap: 10px;
    min-width: 0;
    padding: 7px 10px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-chrome);
  }
  .choice.unavailable {
    cursor: not-allowed;
  }
  .choice.unavailable > :not(input) {
    opacity: 0.56;
  }
  .choice input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .radio-dot {
    width: 14px;
    height: 14px;
    border: 1px solid var(--field-edge);
    border-radius: var(--r-full);
  }
  .choice:has(> input:checked) .radio-dot {
    border: 4px solid var(--brand-strong);
    background: var(--surface-1);
  }
  .copy {
    display: grid;
    gap: 1px;
    min-width: 0;
    line-height: 1.35;
  }
  .copy strong {
    font-size: var(--type-sm);
    font-weight: var(--w-strong);
    overflow-wrap: anywhere;
  }
  .copy small {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .fold {
    justify-self: start;
    min-height: 24px;
    padding-inline: var(--space-3);
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
  @media (hover: hover) {
    .fold:hover {
      color: var(--text);
    }
  }
  .btn {
    justify-self: start;
  }
  .validation {
    display: grid;
    grid-template-columns: 8px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    min-height: 24px;
    padding-inline: var(--space-3);
    font-size: var(--type-xs);
  }
  .validation-copy {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0 6px;
    min-width: 0;
  }
  .validation-copy strong {
    flex: none;
    font-weight: var(--w-heavy);
  }
  .validation-copy small {
    min-width: 0;
    color: var(--text-soft);
    font-size: inherit;
  }
</style>
