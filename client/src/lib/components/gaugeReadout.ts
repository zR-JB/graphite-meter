import { fmtMs, reasonLabel } from "../format";
import type { Phase, RunnerError } from "../runner/contract";
import type { PreparationState } from "../state/store.svelte";
import { MISSING, phaseLabel } from "../presentation/vocabulary";
import { preparationFailurePresentation } from "./preparationFailure";
import type { ResultGaugeArc } from "./resultGauge";

export interface GaugeReadoutInput {
  phase: Phase;
  running: boolean;
  preparing: boolean;
  preparation: PreparationState;
  startError: string;
  error: RunnerError | null;
  aggregateEvidence: boolean;
  latencyTimeout: boolean;
  latencyMs: number;
  hasLatencyResult: boolean;
  unusable: boolean;
  arcs: ResultGaugeArc[];
  headline: ResultGaugeArc | null;
  /** Animated for display; never announced. */
  animatedBytesPerSec: number;
  measuredBytesPerSec: number;
  rate: (bytesPerSec: number) => string;
  unit: string;
}

const EMPTY = { value: MISSING, unit: "" };
const AWAITING = { value: MISSING, unit: "awaiting server windows" };
const transfer = (phase: Phase) =>
  phase === "download" || phase === "upload" || phase === "bidirectional";

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
  if (!transfer(phase)) return EMPTY;
  if (!input.aggregateEvidence) return AWAITING;
  return { value: input.rate(input.animatedBytesPerSec), unit: input.unit };
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
  const display = displayed(input);
  const announced =
    !input.aggregateEvidence && input.running
      ? AWAITING
      : transfer(phase)
        ? { value: input.rate(input.measuredBytesPerSec), unit: input.unit }
        : display;
  const arc = phase === "complete" ? input.headline : null;
  const terminal = arc && { ...arc, value: input.rate(arc.bytesPerSec) };
  const preparationLabel =
    preparation.status === "authenticating"
      ? "Checking sign-in"
      : phaseLabel("connecting");
  const failure = preparationFailurePresentation(preparation, input.startError);
  const status = terminalStatus(input);
  const hint = input.preparing
    ? preparationLabel
    : phase === "idle" || phase === "connecting" || phase === "warmup"
      ? phaseLabel(phase)
      : "";
  const path = (state: string) => (state === "disabled" ? "not needed" : state);
  const statusText = input.preparing
    ? `${preparationLabel}. Throughput path ${path(preparation.throughput)}; Latency path ${path(preparation.latency)}`
    : failure
      ? `${failure.headline} — ${failure.detail}`
      : status
        ? `${status.headline} — ${status.action}`
        : hint;
  const results =
    phase === "complete"
      ? input.arcs
          .map(
            (arc) =>
              `${arc.label} ${input.rate(arc.bytesPerSec)} ${input.unit}${arc.dashed ? ", partial" : ""}`,
          )
          .join("; ")
      : "";
  return {
    display,
    terminal,
    preparationLabel,
    failure,
    status,
    hint,
    announcement:
      statusText ||
      results ||
      `${announced.value} ${announced.unit}, phase ${phase}`,
    announced,
  };
}
