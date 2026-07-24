// Pure phase-to-copy mapping behind PhaseToast.svelte. The eyebrow (kicker)
// and the plain-language message are functions of the phase alone; the error
// message additionally takes the already-resolved reason label so this module
// never reaches into the store.
import type { Phase } from "../runner/contract";

export function phaseKicker(p: Phase): string {
  switch (p) {
    case "connecting":
      return "Connecting";
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

// errorLabel is reasonLabel(store.error.reason), or null when the phase is
// "error" without a captured reason.
export function phaseMessage(p: Phase, errorLabel: string | null): string {
  switch (p) {
    case "connecting":
      return "Verifying selected transport";
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
      return errorLabel ?? "Runner needs attention";
    default:
      return "Ready";
  }
}
