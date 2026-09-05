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
  fill: number;
  warming: boolean;
  failure: boolean;
  hasUsableResult: boolean;
}

interface StagePresentationInput {
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
  let status: StagePresentationStatus = "pending";
  let fill = 0;
  let warming = false;
  if (!input.configured) status = "disabled";
  else if (input.hasFailure) {
    status = input.hasUsableResult ? "partial" : "failed";
    fill = input.hasUsableResult ? 100 : 0;
  } else if (input.hasUsableResult) {
    status = "complete";
    fill = 100;
  } else if (
    input.phaseStage === stage &&
    (input.phase === "warmup" || input.phase === stage)
  ) {
    warming = input.phase === "warmup";
    status = input.measuring ? "active" : "recovering";
    fill = warming ? 0 : Math.round(input.phaseFraction * 200) / 2;
  }
  return { ...base, status, fill, warming };
}
