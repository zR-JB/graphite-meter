// StageTrack projects the shared stage presentation into rail styling. It does not infer result/failure status itself.
import type { FailureReason, Phase, TransportRole } from "../runner/contract";
import type {
  StagePresentation,
  StagePresentationStatus,
} from "../state/stagePresentation";
import type { StageKey } from "../state/store.svelte";
import { STAGE, STATUS, reasonLabel } from "../presentation/vocabulary";

type SegState = StagePresentationStatus | "warmup";

export interface Segment {
  state: SegState;
  fill: number;
}

/* Selection belongs to the editable next-run configuration; execution belongs to the retained run. */
interface StageTrackModel extends Segment {
  selected: boolean;
  tag: string | null;
  locked: boolean;
  execution: StagePresentation;
}

function segmentState(stage: StagePresentation): Segment {
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
  const segment = !selected
    ? { state: "disabled" as const, fill: 0 }
    : execution.status === "disabled"
      ? { state: "pending" as const, fill: 0 }
      : segmentState(execution);
  const tag = !selected
    ? STATUS["not-run"]
    : execution.status === "disabled"
      ? STATUS.next
      : execution.status === "partial" || execution.status === "failed"
        ? STATUS[execution.status]
        : null;
  return {
    selected,
    locked,
    execution,
    ...segment,
    tag,
  };
}

/** Bidirectional shows while Settings includes it or the retained run executed it. */
export const stageShown = (
  stage: StageKey,
  selected: boolean,
  execution: StagePresentation,
) => stage !== "bidirectional" || selected || execution.status !== "disabled";

// Why a locked segment cannot be toggled, or null when it can.
export function lockReason(
  canToggle: boolean,
  phase: Phase,
  phaseStage: TransportRole | null,
  stage: StageKey,
  state: SegState,
): string | null {
  if (canToggle) return null;
  if (state === "complete") return null;
  if (state === "partial") return STATUS.partial;
  if (phaseStage === stage)
    return state === "recovering" ? STATUS.recovering : STATUS.running;
  return phase === "complete" ? STATUS.complete : STATUS.upcoming;
}

const RESULT_STATES: Partial<Record<SegState, string>> = {
  complete: STATUS.complete,
  partial: STATUS.partial,
  failed: STATUS.failed,
};

/** Title, then what the stage holds (its result, failure or status), then what a toggle does. */
export function stageTip(input: {
  stage: StageKey;
  selected: boolean;
  locked: boolean;
  state: SegState;
  reason: string | null;
  failure: FailureReason | null;
  value: string | null;
}): string {
  const { stage, selected, locked, state, reason, failure, value } = input;
  const settled = RESULT_STATES[state];
  const status = settled
    ? [settled, value, failure && reasonLabel(failure)]
        .filter(Boolean)
        .join(" · ")
    : (reason ?? STAGE[stage].about);
  const action = locked
    ? ["active", "recovering", "warmup"].includes(state)
      ? "Locked while it runs"
      : state === "pending"
        ? "Locked while the test starts"
        : "Locked until the run ends"
    : !selected
      ? "Toggle to include"
      : stage === "bidirectional"
        ? "Toggle to skip; Settings brings it back"
        : settled
          ? "Toggle to skip next run"
          : "Toggle to skip";
  return `${STAGE[stage].label}\n${status}\n${action}`;
}
