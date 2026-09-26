import type { Phase, StageStatus, TransportRole } from "../runner/contract";

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
}

interface StagePresentationInput {
  configured: boolean;
  /** The completed run's own status, which settles every stage. */
  settled: StageStatus | undefined;
  phase: Phase;
  phaseStage: TransportRole | null;
  phaseFraction: number;
  measuring: boolean;
  hasResult: boolean;
  hasFailure: boolean;
  finished: boolean;
}

export function deriveStagePresentation(
  stage: TransportRole,
  input: StagePresentationInput,
): StagePresentation {
  let status: StagePresentationStatus = "pending";
  let fill = 0;
  let warming = false;
  if (!input.configured || input.settled === "not-run") status = "disabled";
  else if (input.settled) status = input.settled;
  else if (input.hasFailure) status = input.hasResult ? "partial" : "failed";
  else if (input.hasResult) status = "complete";
  else if (
    input.phaseStage === stage &&
    (input.phase === "warmup" || input.phase === stage)
  ) {
    warming = input.phase === "warmup";
    status = input.measuring ? "active" : "recovering";
    fill = warming ? 0 : Math.round(input.phaseFraction * 200) / 2;
  } else if (input.finished) status = "failed";
  if (status === "complete" || status === "partial") fill = 100;
  return {
    stage,
    configured: input.configured,
    status,
    fill,
    warming,
    failure: input.hasFailure,
  };
}
