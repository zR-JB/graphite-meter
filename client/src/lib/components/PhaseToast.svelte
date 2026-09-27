<script lang="ts">
  import Icon from "./Icon.svelte";
  import { store } from "../state/store.svelte";
  import {
    CONNECTIVITY,
    STATUS_TONE,
    phaseLabel,
    reasonLabel,
  } from "../presentation/vocabulary";
  import { serverIssues } from "../presentation/resultSummary";
  import { announceChanges } from "../presentation/announcer.svelte";

  const LINGER_MS = 3200;

  const stalled = $derived(store.effectiveConnectivity === "recovering");
  const stall = $derived({
    kicker: CONNECTIVITY.recovering.label,
    message: reasonLabel(store.stallInfo?.reason ?? "connection-lost"),
    tone: CONNECTIVITY.recovering.tone,
  });
  const issues = $derived(
    store.serverDetails ? serverIssues(store.serverDetails) : [],
  );
  const issue = $derived(
    issues.length > 1
      ? `${issues.length} measurement issues — details under the results`
      : issues.map(({ server, text }) => `${server} · ${text}`).join(""),
  );

  // Only failures and issues: the status bar already names every phase.
  let toast = $state({ kicker: "", message: "", tone: "" });
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
    else if (added)
      show({ kicker: "Issue", message: issue, tone: STATUS_TONE.failed });
    else if (phase === "error")
      show({
        kicker: phaseLabel(phase),
        message: error ? reasonLabel(error.reason) : "",
        tone: STATUS_TONE.failed,
      });
  });
  $effect(() => () => clearTimeout(timer));
  const shown = $derived(stalled ? stall : toast);
  announceChanges(() => (stalled ? `${stall.kicker}: ${stall.message}` : ""));
  announceChanges(() => issue);
</script>

<div
  class="float phase-toast"
  class:visible={visible || stalled}
  data-tone={shown.tone}
  aria-hidden="true"
>
  <span class="notice-icon"><Icon name="info" /></span>
  <span class="kicker">{shown.kicker}</span>
  <strong>{shown.message}</strong>
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
    color: var(--tone);
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
