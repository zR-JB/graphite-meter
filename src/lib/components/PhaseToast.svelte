<script lang="ts">
  /* ============================================================
   * <PhaseToast> — transient phase-change announcer (§13.7)
   * A fixed, bottom-right toast that surfaces a contextual message
   * each time `store.phase` changes, then auto-dismisses (~1.35s,
   * ~2.2s on complete). role="status" + aria-live="polite" gives
   * screen readers a calm, per-transition announcement (the gauge
   * a11y mirror in ReactorStage handles the per-value detail).
   *
   * Reactivity: a single `$effect` watches `store.phase`; the kicker
   * (eyebrow) and message are pure functions of the current phase.
   * Tokens only. Reduced-motion: the slide/scale is dropped (handled
   * by the global §4.5 guard) so it just fades / appears.
   * ============================================================ */
  import type { RunnerError } from "../runner/contract";
  import { console as store } from "../state/console.svelte";

  let visible = $state(false);
  let prevPhase = store.phase;
  let timer: ReturnType<typeof setTimeout> | null = null;

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

  /** Friendly phrasing for a structured failure reason. */
  function reasonLabel(reason: RunnerError["reason"]): string {
    switch (reason) {
      case "preflight-failed":
        return "Couldn't reach the server";
      case "connection-lost":
        return "Connection lost";
      case "timeout":
        return "Connection timed out";
      case "protocol-error":
        return "Unexpected server response";
      case "transport-unavailable":
        return "Couldn't establish a connection";
      case "internal-error":
        return "Runner needs attention";
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
    // Linger longer on the terminal "complete" state; otherwise a brisk peek.
    timer = setTimeout(() => (visible = false), phase === "complete" ? 2200 : 1350);

    return () => {
      if (timer) clearTimeout(timer);
    };
  });
</script>

<div class="phase-toast" class:visible role="status" aria-live="polite">
  <span class="kicker">{kicker(store.phase)}</span>
  <strong>{message(store.phase)}</strong>
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

  @media (max-width: 759px) {
    .phase-toast {
      right: 12px;
      left: 12px;
      bottom: 40px;
      min-width: 0;
    }
  }
</style>
