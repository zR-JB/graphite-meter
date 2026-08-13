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

/**
 * Selection belongs to the editable next-run configuration; execution belongs
 * to the retained run. Keeping both in this model prevents a terminal rail
 * toggle from rewriting the status of evidence that was already collected.
 */
export interface StageTrackModel extends Segment {
  selected: boolean;
  tag: string | null;
  locked: boolean;
  execution: StagePresentation;
}

export function segmentState(stage: StagePresentation): Segment {
  return {
    state: stage.warming ? "warmup" : stage.status,
    fill: stage.fill,
  };
}

export function stageTrackModel(input: {
  selected: boolean;
  locked: boolean;
  execution: StagePresentation;
}): StageTrackModel {
  const { selected, locked, execution } = input;
  if (!selected) {
    return {
      selected,
      locked,
      execution,
      state: "disabled",
      fill: 0,
      tag: "skipped",
    };
  }

  // A stage enabled after a terminal run was not part of that execution. It is
  // selected for the next run, but has no historical result to project.
  if (execution.status === "disabled") {
    return {
      selected,
      locked,
      execution,
      state: "pending",
      fill: 0,
      tag: "next run",
    };
  }

  const segment = segmentState(execution);
  return {
    selected,
    locked,
    execution,
    ...segment,
    tag:
      execution.status === "partial" || execution.status === "failed"
        ? execution.status
        : null,
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
