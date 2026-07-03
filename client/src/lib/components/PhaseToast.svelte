<script lang="ts">
  /* ============================================================
   * <PhaseToast> — transient phase-change announcer (§13.7)
   * A fixed, bottom-right toast that surfaces a contextual message
   * each time `store.phase` changes, then auto-dismisses (~1.35s,
   * ~2.2s on complete). role="status" + aria-live="polite" gives
   * screen readers a calm, per-transition announcement (the gauge
   * a11y mirror in GaugePanel handles the per-value detail).
   *
   * Reactivity: a single `$effect` watches `store.phase`; the kicker
   * (eyebrow) and message are pure functions of the current phase.
   * Tokens only. Reduced-motion: the slide/scale is dropped (handled
   * by the global §4.5 guard) so it just fades / appears.
   * ============================================================ */
  import { untrack } from "svelte";
  import { store } from "../state/store.svelte";
  import { reasonLabel } from "../format";

  let visible = $state(false);
  let prevPhase = store.phase;
  let timer: ReturnType<typeof setTimeout> | null = null;

  // A skipped stage takes over the toast briefly (err-tinted): it usually
  // coincides with the next stage's phase transition, so it gets priority
  // over the routine phase message until its timer clears it.
  let skipMsg = $state<string | null>(null);
  let prevFailCount = 0;
  const STAGE_LABEL: Record<string, string> = {
    latency: "Latency",
    download: "Download",
    upload: "Upload",
    bidirectional: "Bi-dir",
  };

  // A stall (connection lost) takes over the toast for as long as it lasts —
  // it is not a phase transition, so it has its own sticky visibility. The
  // moment the link resumes (`measuring` true) it clears and the normal
  // phase-change toast resumes. Reads store.stallInfo for the reason copy.
  const stalled = $derived(store.isRunning && !store.measuring);
  const stallMessage = $derived.by(() => {
    const info = store.stallInfo;
    // Prefer the backend's human detail; else the friendly reason phrase.
    const tail = info?.detail ?? (info ? reasonLabel(info.reason) : "the link dropped");
    return `Connection lost — ${tail}`;
  });

  /** Short uppercase eyebrow naming the lifecycle stage. */
  function kicker(p: typeof store.phase): string {
    switch (p) {
      case "warmup":
        return "Warmup";
      case "latency":
        return "Latency";
      case "download":
        return "Download";
      case "upload":
        return "Upload";
      case "bidirectional":
        return "Bidirectional";
      case "complete":
        return "Complete";
      case "aborted":
        return "Aborted";
      case "error":
        return "Error";
      default:
        return "Standby";
    }
  }

  /** Plain-language message for the active phase. */
  function message(p: typeof store.phase): string {
    switch (p) {
      case "warmup":
        return "Calibrating transport";
      case "latency":
        return "Measuring path latency";
      case "download":
        return "Receiving stream";
      case "upload":
        return "Sending stream";
      case "bidirectional":
        return "Sending + receiving";
      case "complete":
        return "Complete";
      case "aborted":
        return "Sequence stopped";
      case "error":
        return store.error ? reasonLabel(store.error.reason) : "Runner needs attention";
      default:
        return "Ready";
    }
  }

  $effect(() => {
    const phase = store.phase;
    if (phase === prevPhase) return;
    prevPhase = phase;

    visible = true;
    if (timer) clearTimeout(timer);
    // Linger longer on terminal states — complete, and especially aborted/error
    // (the run just ended unexpectedly; a 1.35s blink undersells that) — or
    // while a skip notice holds the toast; otherwise a brisk peek.
    const linger = untrack(() => skipMsg)
      ? 3200
      : phase === "aborted" || phase === "error"
        ? 3200
        : phase === "complete"
          ? 2200
          : 1350;
    timer = setTimeout(() => {
      visible = false;
      skipMsg = null;
    }, linger);

    return () => {
      if (timer) clearTimeout(timer);
    };
  });

  $effect(() => {
    const fails = Object.values(store.stageFailures);
    if (fails.length > prevFailCount) {
      const f = fails[fails.length - 1];
      skipMsg = `${STAGE_LABEL[f.stage]} skipped — ${f.message}`;
      visible = true;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        visible = false;
        skipMsg = null;
      }, 3200);
    }
    prevFailCount = fails.length;
  });

  // While stalled the toast is sticky (no auto-dismiss): it stays up for the
  // whole dead-air window and clears the instant the link resumes. Cancel any
  // pending auto-dismiss timer so a phase toast doesn't hide the stall notice.
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
    (visible && (skipMsg != null || store.phase === "error" || store.phase === "aborted"))}
  role="status"
  aria-live="polite"
>
  <span class="kicker">{stalled ? "Link" : skipMsg ? "Skipped" : kicker(store.phase)}</span>
  <strong>{stalled ? stallMessage : (skipMsg ?? message(store.phase))}</strong>
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

  /* Reduced motion: no slide/scale; the global §4.5 guard collapses the
     transition, but pin the resting transform so it never animates in. */
  @media (prefers-reduced-motion: reduce) {
    .phase-toast {
      transform: none;
    }
    .phase-toast.visible {
      transform: none;
    }
  }

  @media (max-width: 759px) { /* bp: stacked */
    .phase-toast {
      right: 12px;
      left: 12px;
      bottom: 40px;
      min-width: 0;
    }
    /* Routine per-phase-transition toasts duplicate what StatusBar already
       shows textually in the footer, and fire ~5-6 times per run — on a
       phone that reads as the notification obstructing the experience.
       The stall/error/aborted (.alert) toast is the one case nothing else
       on screen surfaces, so it stays visible here. */
    .phase-toast:not(.alert) {
      display: none;
    }
  }
</style>
