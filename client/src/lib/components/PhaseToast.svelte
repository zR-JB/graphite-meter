<script lang="ts">
  /* Transient phase-change announcer, pinned bottom-right: one message per
     `store.phase` change, then auto-dismiss. role="status" with
     aria-live="polite" gives screen readers one calm announcement per
     transition. GaugePanel's mirror carries the per-value detail. */
  import { ICON } from "../constants";
  import { untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import { reasonLabel } from "../format";
  import { phaseKicker, phaseMessage } from "./phasePresentation";
  import { failureDetail } from "./failurePresentation";

  // A terminal or skipped-stage notice earns a longer read than a routine
  // phase blink.
  const LINGER_ALERT_MS = 3200;
  const LINGER_COMPLETE_MS = 2200;
  const LINGER_PHASE_MS = 1350;

  function lingerMs(phase: typeof store.phase, skipped: boolean): number {
    if (skipped || phase === "aborted" || phase === "error")
      return LINGER_ALERT_MS;
    return phase === "complete" ? LINGER_COMPLETE_MS : LINGER_PHASE_MS;
  }

  let visible = $state(false);
  let prevPhase = store.phase;
  let timer: ReturnType<typeof setTimeout> | null = null;

  // A skipped stage takes over the toast briefly, err-tinted. It coincides
  // with the next stage's transition, so it outranks the routine phase
  // message until its timer clears.
  let skipMessage = $state<string | null>(null);
  let prevFailCount = 0;
  const STAGE_LABEL: Record<string, string> = {
    latency: "Latency",
    download: "Download",
    upload: "Upload",
    bidirectional: "Bi-dir",
  };

  // A stall (connection lost) holds the toast for its whole duration. It is
  // not a phase transition, so it owns its own visibility and clears the
  // moment `measuring` goes true. store.stallInfo carries the reason copy.
  const stalled = $derived(store.isRunning && !store.measuring);
  const stallMessage = $derived.by(() => {
    const info = store.stallInfo;
    // Prefer the backend's human detail; else the friendly reason phrase.
    const tail = info
      ? failureDetail(info.detail, reasonLabel(info.reason))
      : "the link dropped";
    return `Connection lost — ${tail}`;
  });

  const message = (p: typeof store.phase): string =>
    phaseMessage(
      p,
      store.error ? reasonLabel(store.error.reason) : null,
      store.result?.outcome,
    );

  $effect(() => {
    const phase = store.phase;
    if (phase === prevPhase) return;
    prevPhase = phase;

    visible = true;
    if (timer) clearTimeout(timer);
    const linger = lingerMs(phase, untrack(() => skipMessage) != null);
    timer = setTimeout(() => {
      visible = false;
      skipMessage = null;
    }, linger);

    return () => {
      if (timer) clearTimeout(timer);
    };
  });

  $effect(() => {
    const failures = store.serverDetails?.failures.length
      ? store.serverDetails.failures.map((failure) => {
          const name =
            store.serverDetails?.selection.find(
              (server) => server.id === failure.serverId,
            )?.name ?? "Server";
          return `${name}: ${STAGE_LABEL[failure.stage] ?? failure.stage} unavailable`;
        })
      : Object.values(store.stageFailures).map(
          (failure) =>
            `${STAGE_LABEL[failure.stage]} skipped — ${failureDetail(failure.message)}`,
        );
    if (failures.length > prevFailCount) {
      skipMessage =
        failures.length > 1
          ? `${failures.length} measurement issues — details in results`
          : failures[failures.length - 1];
      visible = true;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        visible = false;
        skipMessage = null;
      }, LINGER_ALERT_MS);
    }
    prevFailCount = failures.length;
  });
</script>

<div
  class="phase-toast"
  class:visible={visible || stalled}
  class:alert={stalled ||
    (visible &&
      (skipMessage != null ||
        store.phase === "error" ||
        store.phase === "aborted"))}
  role="status"
  aria-live="polite"
>
  <span class="notice-icon" aria-hidden="true"
    >{#if stalled || skipMessage || store.phase === "error"}{@html ICON.info}{:else if store.phase === "complete"}{@html ICON.check}{:else}{@html ICON.ping}{/if}</span
  >
  <span class="kicker"
    >{stalled
      ? "Link"
      : skipMessage
        ? "Issue"
        : phaseKicker(store.phase, store.result?.outcome)}</span
  >
  <strong
    >{stalled ? stallMessage : (skipMessage ?? message(store.phase))}</strong
  >
</div>

<style>
  .phase-toast {
    position: fixed;
    right: 18px;
    bottom: 40px;
    z-index: 50;
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr);
    column-gap: 9px;
    align-items: center;
    min-width: 220px;
    max-width: min(360px, calc(100vw - 24px));
    pointer-events: none;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: var(--shadow-float);
    padding: var(--space-2) var(--space-3);
    opacity: 0;
    transform: translateY(4px);
    transition:
      opacity var(--dur-slide) var(--ease-out),
      transform var(--dur-slide) var(--ease-out);
  }
  .phase-toast.visible {
    opacity: 1;
    transform: translateY(0);
  }
  /* Keep issue emphasis on the icon, with the same calm surface. */
  .phase-toast.alert {
    border-color: var(--border-strong);
  }
  .phase-toast.alert .notice-icon {
    color: var(--err);
  }

  .notice-icon {
    grid-row: 1 / 3;
    display: grid;
    place-items: center;
    color: var(--text-muted);
  }
  .notice-icon :global(svg) {
    width: 18px;
    height: 18px;
  }
  .kicker {
    color: var(--text-muted);
    font-family: var(--font-sans);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0;
  }
  strong {
    margin-top: 2px;
    color: var(--text);
    font-size: 12px;
    font-weight: 500;
    line-height: 1.4;
    overflow-wrap: anywhere;
  }

  /* Reduced motion: the resting transform is pinned, so the toast fades in
     without a slide or scale. */
  @media (prefers-reduced-motion: reduce) {
    .phase-toast {
      transform: none;
    }
    .phase-toast.visible {
      transform: none;
    }
  }

  @media (max-width: 759px) {
    /* bp: stacked */
    .phase-toast {
      right: 12px;
      left: 12px;
      bottom: 40px;
      min-width: 0;
    }
    /* Routine phase toasts duplicate the StatusBar footer and fire 5 to 6
       times per run, which on a phone reads as an obstruction. The .alert
       toast (stall, error, aborted) is the one state nothing else surfaces. */
    .phase-toast:not(.alert) {
      display: none;
    }
  }
</style>
