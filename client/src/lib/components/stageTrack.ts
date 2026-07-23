// Pure stage-rail derivation split out of StageTrack.svelte so bun tests can
// exercise it without the rune store. The component reads store.phase,
// phaseFraction, phaseStage, and the toggle guards and passes them in; these
// functions own the pending/warmup/active/done/failed decision only.
import type { Phase, TransportRole } from "../runner/contract";
import type { StageKey } from "../state/store.svelte";

export const TRACK_ORDER = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const;

export type SegState =
  "disabled" | "warmup" | "active" | "done" | "failed" | "pending";

export interface Segment {
  state: SegState;
  fill: number;
}

export function stageIndex(stage: TransportRole | null): number {
  return stage ? TRACK_ORDER.indexOf(stage) : -1;
}

export function progressFill(phaseFraction: number): number {
  return Math.round(phaseFraction * 200) / 2;
}

// curI is stageIndex(phaseStage); enabled/failed come from the store config.
export function segmentState(
  phase: Phase,
  phaseFraction: number,
  stage: StageKey,
  enabled: boolean,
  failed: boolean,
  curI: number,
): Segment {
  if (!enabled) return { state: "disabled", fill: 0 };
  if (failed) return { state: "failed", fill: 0 };
  if (phase === "complete") return { state: "done", fill: 100 };
  const stI = TRACK_ORDER.indexOf(stage);
  if (phase === "warmup") {
    if (stI < curI) return { state: "done", fill: 100 };
    if (stI === curI) return { state: "warmup", fill: 0 };
    return { state: "pending", fill: 0 };
  }
  if (curI === -1) return { state: "pending", fill: 0 };
  if (stI < curI) return { state: "done", fill: 100 };
  if (stI === curI)
    return { state: "active", fill: progressFill(phaseFraction) };
  return { state: "pending", fill: 0 };
}

// Bidirectional is the terminal stage, so it has no "later than current" case.
export function bidirectionalState(
  phase: Phase,
  phaseFraction: number,
  phaseStage: TransportRole | null,
  enabled: boolean,
  failed: boolean,
): Segment | null {
  if (!enabled) return null;
  if (failed) return { state: "failed", fill: 0 };
  if (phase === "complete") return { state: "done", fill: 100 };
  if (phase === "warmup" && phaseStage === "bidirectional")
    return { state: "warmup", fill: 0 };
  if (phase === "bidirectional")
    return { state: "active", fill: progressFill(phaseFraction) };
  return { state: "pending", fill: 0 };
}

// Why a locked segment cannot be toggled, or null when it can. canToggle is
// store.canToggleStage(stage).
export function lockReason(
  canToggle: boolean,
  phase: Phase,
  phaseStage: TransportRole | null,
  stage: StageKey,
  state: SegState,
): string | null {
  if (canToggle) return null;
  if (state === "done") return "done";
  if (phase === stage) return "running";
  const curI = stageIndex(phaseStage);
  const stI = TRACK_ORDER.indexOf(stage);
  return curI >= 0 && stI < curI ? "done" : "upcoming";
}
