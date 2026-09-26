<script lang="ts">
  // Visual only: GaugePanel announces phases.
  import { ICON } from "../constants";
  import { untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtDuration, reasonLabel } from "../format";
  import { failureDetail } from "./failurePresentation";
  import { STAGE_ORDER } from "../state/stagePresentation";
  import { STAGE, phaseLabel } from "../presentation/vocabulary";
  import { serverName } from "../presentation/serverAppearance";
  import { announceChanges } from "../presentation/announcer.svelte";

  const LINGER_ALERT_MS = 3200;
  const LINGER_COMPLETE_MS = 2200;
  const LINGER_PHASE_MS = 1350;

  const stalled = $derived(store.isRunning && !store.measuring);
  const stallMessage = $derived(
    `Connection lost — ${
      store.stallInfo
        ? failureDetail(
            store.stallInfo.detail,
            reasonLabel(store.stallInfo.reason),
          )
        : "the link dropped"
    }`,
  );
  const stages = $derived(
    STAGE_ORDER.filter((stage) => store.runConfig.stages[stage]),
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
  const notice = $derived.by(() => {
    const { phase, result, error, phaseStage } = store;
    const label = phaseLabel(phase, result?.outcome);
    if (phase === "complete" && result)
      return {
        kicker: label,
        message: `${fmtDuration(result.durationMs)} · ${fmtBytes(store.bytesTransferred, store.unitBase)} transferred`,
      };
    if (phase === "error")
      return { kicker: label, message: error ? reasonLabel(error.reason) : "" };
    if (phase === "aborted")
      return { kicker: label, message: "Run again to restart" };
    if (phaseStage && phase !== "warmup")
      return {
        kicker: `Stage ${stages.indexOf(phaseStage) + 1} of ${stages.length}`,
        message: label,
      };
    return { kicker: "Preparing", message: label };
  });

  // One timer: an issue holds the toast until it lapses; idle clears it.
  let toast = $state<{ issue: boolean } | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seenPhase = store.phase;
  let seenIssues = 0;
  function show(next: typeof toast, linger = 0) {
    clearTimeout(timer);
    toast = next;
    if (next) timer = setTimeout(() => (toast = null), linger);
  }
  $effect(() => {
    const { phase } = store;
    const count = issues.length;
    const phaseChanged = phase !== seenPhase;
    const issueAdded = count > seenIssues;
    seenPhase = phase;
    seenIssues = count;
    if (phase === "idle") show(null);
    else if (issueAdded) show({ issue: true }, LINGER_ALERT_MS);
    else if (phaseChanged && !untrack(() => toast?.issue))
      show(
        { issue: false },
        phase === "aborted" || phase === "error"
          ? LINGER_ALERT_MS
          : phase === "complete"
            ? LINGER_COMPLETE_MS
            : LINGER_PHASE_MS,
      );
  });
  $effect(() => () => clearTimeout(timer));
  announceChanges(() => (stalled ? stallMessage : ""));
  announceChanges(() => issue);
</script>

<div
  class="float phase-toast"
  class:visible={toast || stalled}
  class:alert={stalled ||
    toast?.issue ||
    (toast && (store.phase === "error" || store.phase === "aborted"))}
  aria-hidden="true"
>
  <span class="notice-icon">
    {#if stalled || toast?.issue || store.phase === "error"}
      {@html ICON.info}
    {:else if store.phase === "complete"}
      {@html ICON.check}
    {:else}
      {@html ICON.ping}
    {/if}
  </span>
  <span class="kicker"
    >{stalled ? "Connection" : toast?.issue ? "Issue" : notice.kicker}</span
  >
  <strong
    >{stalled ? stallMessage : toast?.issue ? issue : notice.message}</strong
  >
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
    color: var(--text-muted);
  }
  /* Issue emphasis stays on the icon, with the same calm surface. */
  .alert .notice-icon {
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
    /* On phones only the alert toast shows; routine phases duplicate the status bar. */
    .phase-toast:not(.alert) {
      display: none;
    }
  }
</style>
