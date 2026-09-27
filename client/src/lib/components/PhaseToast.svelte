<script lang="ts">
  import Icon from "./Icon.svelte";
  import { store } from "../state/store.svelte";
  import { STAGE, phaseLabel, reasonLabel } from "../presentation/vocabulary";
  import { serverName } from "../presentation/serverAppearance";
  import { announceChanges } from "../presentation/announcer.svelte";

  const LINGER_MS = 3200;

  const stalled = $derived(store.isRunning && !store.measuring);
  const stallMessage = $derived(
    `Connection lost — ${
      store.stallInfo ? reasonLabel(store.stallInfo.reason) : "the link dropped"
    }`,
  );
  const issues = $derived.by(() => {
    const details = store.serverDetails;
    return (details?.failures ?? []).map(
      (failure) =>
        `${serverName(details!.selection, failure.serverId)}: ${STAGE[failure.stage].label} unavailable`,
    );
  });
  const issue = $derived(
    issues.length > 1
      ? `${issues.length} measurement issues — details in results`
      : (issues[0] ?? ""),
  );

  // Only failures and issues: the status bar already names every phase.
  let toast = $state({ kicker: "", message: "" });
  let visible = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seenIssues = 0;
  function show(next: typeof toast | null) {
    clearTimeout(timer);
    visible = !!next;
    if (!next) return;
    toast = next;
    // Not motion: a toast lingers for its reading time.
    timer = setTimeout(() => (visible = false), LINGER_MS);
  }
  $effect(() => {
    const { phase, error } = store;
    const added = issues.length > seenIssues;
    seenIssues = issues.length;
    if (phase === "idle") show(null);
    else if (added) show({ kicker: "Issue", message: issue });
    else if (phase === "error")
      show({
        kicker: phaseLabel(phase),
        message: error ? reasonLabel(error.reason) : "",
      });
  });
  $effect(() => () => clearTimeout(timer));
  announceChanges(() => (stalled ? stallMessage : ""));
  announceChanges(() => issue);
</script>

<div
  class="float phase-toast"
  class:visible={visible || stalled}
  aria-hidden="true"
>
  <span class="notice-icon"><Icon name="info" /></span>
  <span class="kicker">{stalled ? "Connection" : toast.kicker}</span>
  <strong>{stalled ? stallMessage : toast.message}</strong>
</div>

<style>
  .phase-toast {
    position: fixed;
    right: 18px;
    bottom: 40px;
    z-index: var(--z-toast);
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr);
    align-items: center;
    column-gap: 9px;
    min-width: 220px;
    max-width: min(360px, calc(100vw - 24px));
    padding: var(--space-2) var(--space-3);
    opacity: 0;
    translate: 0 4px;
    pointer-events: none;
    transition:
      opacity var(--dur-slide) var(--ease-out),
      translate var(--dur-slide) var(--ease-out);
  }
  .phase-toast.visible {
    opacity: 1;
    translate: none;
  }
  .notice-icon {
    grid-row: 1 / 3;
    display: grid;
    place-items: center;
    color: var(--err);
  }
  .notice-icon :global(svg) {
    width: 18px;
    height: 18px;
  }
  .kicker {
    color: var(--text-muted);
    font-size: var(--type-2xs);
    font-weight: var(--w-heavy);
  }
  strong {
    margin-top: 2px;
    font-size: var(--type-sm);
    font-weight: var(--w-normal);
    line-height: 1.4;
    overflow-wrap: anywhere;
  }
  @media (max-width: 759px) {
    .phase-toast {
      inset-inline: 12px;
      min-width: 0;
    }
  }
</style>
