<script lang="ts">
  /* Transient phase-change announcer, pinned bottom-right: one message per
     `store.phase` change, then auto-dismiss. role="status" with
     aria-live="polite" gives screen readers one calm announcement per
     transition. GaugePanel's mirror carries the per-value detail. */
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
    phaseMessage(p, store.error ? reasonLabel(store.error.reason) : null);

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
    const failures = Object.values(store.stageFailures);
    if (failures.length > prevFailCount) {
      const latest = failures[failures.length - 1];
      skipMessage = `${STAGE_LABEL[latest.stage]} skipped — ${failureDetail(latest.message)}`;
      visible = true;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        visible = false;
        skipMessage = null;
      }, LINGER_ALERT_MS);
    }
    prevFailCount = failures.length;
  });

  // Dropping the auto-dismiss timer holds the stall notice for the whole
  // dead-air window, past any phase toast underneath it.
  $effect(() => {
    if (stalled && timer) {
      clearTimeout(timer);
      timer = null;
    }
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
  <span class="kicker"
    >{stalled
      ? "Link"
      : skipMessage
        ? "Skipped"
        : phaseKicker(store.phase)}</span
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
    min-width: 220px;
    pointer-events: none;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: linear-gradient(180deg, var(--surface-2), var(--surface-1));
    box-shadow: var(--shadow-float);
    padding: var(--space-2) var(--space-3);
    opacity: 0;
    transform: translateY(10px) scale(0.985);
    transition:
      opacity var(--dur-slide) var(--ease-out),
      transform var(--dur-slide) var(--ease-out);
  }
  .phase-toast.visible {
    opacity: 1;
    transform: translateY(0) scale(1);
  }
  /* Stalled (link dropped): error-tinted so the transient notice reads as an
     alert, not a routine phase change. */
  .phase-toast.alert {
    border-color: color-mix(in srgb, var(--err) 45%, var(--border));
    background: linear-gradient(180deg, var(--err-soft), var(--surface-1));
  }
  .phase-toast.alert .kicker {
    color: var(--err);
  }

  .kicker {
    color: var(--brand-strong);
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
  strong {
    margin-top: 2px;
    color: var(--text);
    font-size: 12px;
    font-weight: 700;
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
