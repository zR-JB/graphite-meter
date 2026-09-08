// Pure phase-to-copy mapping behind PhaseToast.svelte.
import type { Phase, RunResult } from "../runner/contract";

const KICKERS: Record<Phase, string> = {
  idle: "Standby",
  connecting: "Connecting",
  warmup: "Warmup",
  latency: "Latency",
  download: "Download",
  upload: "Upload",
  bidirectional: "Bidirectional",
  complete: "Complete",
  aborted: "Aborted",
  error: "Error",
};

const MESSAGES: Record<Exclude<Phase, "error">, string> = {
  idle: "Ready",
  connecting: "Verifying selected transport",
  warmup: "Calibrating transport",
  latency: "Measuring path latency",
  download: "Receiving stream",
  upload: "Sending stream",
  bidirectional: "Sending + receiving",
  complete: "Complete",
  aborted: "Sequence stopped",
};

export function completionLabel(outcome: RunResult["outcome"]): string {
  return outcome === "incomplete"
    ? "Incomplete"
    : outcome === "partial"
      ? "Partial"
      : "Complete";
}

export const phaseKicker = (
  phase: Phase,
  outcome?: RunResult["outcome"],
): string => (phase === "complete" ? completionLabel(outcome) : KICKERS[phase]);

// errorLabel is reasonLabel(store.error.reason), or null when the phase is "error" without a captured reason.
export function phaseMessage(
  phase: Phase,
  errorLabel: string | null,
  outcome?: RunResult["outcome"],
): string {
  if (phase === "complete") return completionLabel(outcome);
  return phase === "error"
    ? (errorLabel ?? "Runner needs attention")
    : MESSAGES[phase];
}
