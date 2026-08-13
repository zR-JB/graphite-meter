// One truthful stage-status derivation shared by the instruments. A usable
// result plus a failure is deliberately partial: the failure must not erase
// evidence that the final reducer retained.
import type { Phase, TransportRole } from "../runner/contract";

export const STAGE_ORDER = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const satisfies readonly TransportRole[];

export type StagePresentationStatus =
  | "disabled"
  | "pending"
  | "active"
  | "recovering"
  | "complete"
  | "partial"
  | "failed";

export interface StagePresentation {
  stage: TransportRole;
  configured: boolean;
  status: StagePresentationStatus;
  /** Current stage progress; terminal results are represented as full. */
  fill: number;
  warming: boolean;
  failure: boolean;
  hasUsableResult: boolean;
}

export interface StagePresentationInput {
  configured: boolean;
  phase: Phase;
  phaseStage: TransportRole | null;
  phaseFraction: number;
  measuring: boolean;
  hasUsableResult: boolean;
  hasFailure: boolean;
}

export function deriveStagePresentation(
  stage: TransportRole,
  input: StagePresentationInput,
): StagePresentation {
  const base = {
    stage,
    configured: input.configured,
    failure: input.hasFailure,
    hasUsableResult: input.hasUsableResult,
  };
  if (!input.configured)
    return { ...base, status: "disabled", fill: 0, warming: false };
  if (input.hasFailure)
    return {
      ...base,
      status: input.hasUsableResult ? "partial" : "failed",
      fill: input.hasUsableResult ? 100 : 0,
      warming: false,
    };
  if (input.hasUsableResult)
    return { ...base, status: "complete", fill: 100, warming: false };
  if (input.phaseStage === stage) {
    const warming = input.phase === "warmup";
    return {
      ...base,
      status: input.measuring ? "active" : "recovering",
      fill: warming ? 0 : Math.round(input.phaseFraction * 200) / 2,
      warming,
    };
  }
  return { ...base, status: "pending", fill: 0, warming: false };
}
