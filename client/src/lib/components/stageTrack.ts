// StageTrack projects the shared stage presentation into rail styling. It does
// not infer result/failure status itself.
import type { Phase, TransportRole } from "../runner/contract";
import type {
  StagePresentation,
  StagePresentationStatus,
} from "../state/stagePresentation";
import type { StageKey } from "../state/store.svelte";

export type SegState = StagePresentationStatus | "warmup";

export interface Segment {
  state: SegState;
  fill: number;
}

export function segmentState(stage: StagePresentation): Segment {
  return {
    state: stage.warming ? "warmup" : stage.status,
    fill: stage.fill,
  };
}

// Why a locked segment cannot be toggled, or null when it can. canToggle is
// store.canToggleStage(stage); status comes from the central presentation model.
export function lockReason(
  canToggle: boolean,
  phase: Phase,
  phaseStage: TransportRole | null,
  stage: StageKey,
  state: SegState,
): string | null {
  if (canToggle) return null;
  if (state === "complete" || state === "partial") return "done";
  if (phaseStage === stage)
    return state === "recovering" ? "recovering" : "running";
  return phase === "complete" ? "done" : "upcoming";
}
