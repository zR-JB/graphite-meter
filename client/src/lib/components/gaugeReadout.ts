import { fmtDuration, fmtMs } from "../format";
import type { Phase, RunnerError } from "../runner/contract";
import type { PreparationState } from "../state/store.svelte";
import {
  MISSING,
  phaseLabel,
  reasonLabel,
  statusLabel,
} from "../presentation/vocabulary";
import { preparationFailurePresentation } from "./preparationFailure";
import type { ResultGaugeArc } from "./resultGauge";

export interface GaugeReadoutInput {
  phase: Phase;
  running: boolean;
  preparing: boolean;
  preparation: PreparationState;
  startError: string;
  error: RunnerError | null;
  latencyTimeout: boolean;
  latencyMs: number;
  /** How long no data has arrived while the run stalls; null otherwise. */
  quietMs: number | null;
  hasLatencyResult: boolean;
  unusable: boolean;
  headline: ResultGaugeArc | null;
  rate: (bytesPerSec: number) => string;
  unit: string;
}

const EMPTY = { value: MISSING, unit: "" };
const transfer = (phase: Phase) =>
  phase === "download" || phase === "upload" || phase === "bidirectional";

// Null: the live transfer rate, which the panel formats per frame.
function displayed(input: GaugeReadoutInput) {
  const { phase, headline } = input;
  const latency = { value: fmtMs(input.latencyMs), unit: "ms" };
  if (input.unusable) return EMPTY;
  if (phase === "latency")
    return input.latencyTimeout
      ? { value: MISSING, unit: "probe timeout" }
      : latency;
  if (phase === "complete") {
    if (headline)
      return {
        value: input.rate(headline.bytesPerSec),
        unit: `${input.unit} · ${headline.label}`,
      };
    return input.hasLatencyResult ? latency : EMPTY;
  }
  return transfer(phase) ? null : EMPTY;
}

function terminalStatus({ phase, error }: GaugeReadoutInput) {
  if (phase === "aborted")
    return {
      error: false,
      headline: phaseLabel("aborted"),
      action: "Press Run again to restart",
    };
  if (phase !== "error") return null;
  return {
    error: true,
    headline: error ? reasonLabel(error.reason) : "Something went wrong",
    action: "Press Run again to retry",
  };
}

export function gaugeReadout(input: GaugeReadoutInput) {
  const { phase, preparation } = input;
  const arc = phase === "complete" ? input.headline : null;
  const terminal = arc && { ...arc, value: input.rate(arc.bytesPerSec) };
  const preparationLabel = statusLabel(preparation.status, phase);
  const failure = preparationFailurePresentation(preparation, input.startError);
  const status = terminalStatus(input);
  const hint = input.preparing
    ? preparationLabel
    : phase === "idle" || phase === "connecting" || phase === "warmup"
      ? phaseLabel(phase)
      : "";
  const statusText = failure
    ? `${failure.headline} — ${failure.detail}`
    : status
      ? `${status.headline} — ${status.action}`
      : "";
  // Preparation speaks only through its outcome; the result cards announce a completed run.
  const quiet =
    input.preparing ||
    phase === "idle" ||
    phase === "connecting" ||
    phase === "complete";
  return {
    display: displayed(input),
    terminal,
    preparationLabel,
    failure,
    status,
    hint,
    noData:
      input.quietMs == null
        ? ""
        : `No data for ${fmtDuration(input.quietMs, 0)}`,
    announcement: statusText || (quiet ? "" : phaseLabel(phase)),
  };
}
