// The run's timeline: for each planned stage a warmup then its measured window, on the run's active clock.
import type { Direction } from "../types";

export const STAGES = ["latency", "download", "upload", "bidirectional"] as const;
export type Stage = (typeof STAGES)[number];
export type TransferStage = Exclude<Stage, "latency">;

export interface Plan {
  stages: Record<Stage, boolean>;
  duration: { warmupMs: number } & Record<`${Stage}Ms`, number>;
  /** Transfer stages skip their loaded-latency probes when the latency stage is off. */
  skipLoadedLatencyWhenStageOff: boolean;
}

/** What a stage's warmup and window both exercise; the window reuses its warmup's connections. */
export interface Activity {
  stage: Stage;
  transfer: Direction[];
  loadedLatency: boolean;
}

export interface Segment {
  phase: "warmup" | Stage;
  /** Run-clock milliseconds. */
  start: number;
  end: number;
  activity: Activity;
}

/** A stage runs when it is on and has time; 0 ms skips it. */
export const planned = (plan: Pick<Plan, "stages" | "duration">, stage: Stage) =>
  plan.stages[stage] && plan.duration[`${stage}Ms`] > 0;

const TRANSFER: Record<Stage, Activity["transfer"]> = {
  latency: [],
  download: ["down"],
  upload: ["up"],
  bidirectional: ["down", "up"],
};

/** Each planned stage's activity, in stage order. */
export const activities = (plan: Plan): Activity[] =>
  STAGES.filter((stage) => planned(plan, stage)).map((stage) => ({
    stage,
    transfer: [...TRANSFER[stage]],
    loadedLatency: stage !== "latency" && (planned(plan, "latency") || !plan.skipLoadedLatencyWhenStageOff),
  }));

/** Stages run in order, and a stage may be turned on or off until it begins. */
export const changeable = (current: Stage | null, stage: Stage) =>
  STAGES.indexOf(stage) > (current ? STAGES.indexOf(current) : -1);

/** Slow start fills a typical bandwidth-delay product within ten round trips. */
const SLOW_START_RTTS = 10;
const WARMUP_CEILING_MS = 4_000;

/** `plan` with its configured warmup stretched to ten round trips of `rttMs` but never past 4 s. */
export const adaptWarmup = <T extends Pick<Plan, "duration">>(plan: T, rttMs: number): T => {
  const stretched = Math.round((rttMs > 0 ? rttMs : 0) * SLOW_START_RTTS);
  const warmupMs = Math.min(WARMUP_CEILING_MS, Math.max(plan.duration.warmupMs, stretched));
  return { ...plan, duration: { ...plan.duration, warmupMs } };
};

export function timeline(plan: Plan): Segment[] {
  const segments: Segment[] = [];
  let cursor = 0;
  const push = (phase: Segment["phase"], ms: number, activity: Activity) => {
    if (!(ms > 0)) return;
    segments.push({ phase, start: cursor, end: cursor + ms, activity });
    cursor += ms;
  };
  for (const activity of activities(plan)) {
    push("warmup", plan.duration.warmupMs, activity);
    push(activity.stage, plan.duration[`${activity.stage}Ms`], activity);
  }
  return segments;
}

/** The whole timeline's length. */
export const totalMs = (plan: Plan) => timeline(plan).at(-1)?.end ?? 0;

/** The segment covering `elapsed`, or undefined past the end. */
export const segmentAt = (segments: readonly Segment[], elapsed: number) =>
  segments.find((segment) => elapsed >= segment.start && elapsed < segment.end);

/** The unfinished timeline under a changed plan. What ran stays; the running segment takes its new length (never
    ending before now); stages not started yet follow. `between` means no segment is active. */
export function replan(segments: readonly Segment[], elapsed: number, plan: Plan, between = false): Segment[] {
  const active = between ? undefined : segmentAt(segments, elapsed);
  const kept = active ? segments.filter((s) => s.start < active.start) : segments.filter((s) => s.end <= elapsed);
  if (active) {
    const ms = active.phase === "warmup" ? plan.duration.warmupMs : plan.duration[`${active.phase}Ms`];
    kept.push({ ...active, end: Math.max(elapsed, active.start + ms) });
  }
  let cursor = kept.at(-1)?.end ?? 0;
  for (const next of activities(plan)) {
    // A stage whose window began stays as it is.
    if (kept.some((segment) => segment.phase === next.stage)) continue;
    // A running warmup keeps its activity, so the window reuses its connections and latency policy.
    const warmup = kept.find((segment) => segment.phase === "warmup" && segment.activity.stage === next.stage);
    const activity = warmup?.activity ?? next;
    if (!warmup && plan.duration.warmupMs > 0) {
      kept.push({ phase: "warmup", start: cursor, end: (cursor += plan.duration.warmupMs), activity });
    }
    kept.push({ phase: next.stage, start: cursor, end: (cursor += plan.duration[`${next.stage}Ms`]), activity });
  }
  return kept;
}

/** Ends `active` at `elapsed` and moves the untouched tail forward by the time removed. */
export function truncate(segments: readonly Segment[], active: Segment, elapsed: number): Segment[] {
  const boundary = Math.min(active.end, Math.max(active.start, elapsed));
  const removed = active.end - boundary;
  return segments.map((segment) =>
    segment === active
      ? { ...segment, end: boundary }
      : segment.start >= active.end
        ? { ...segment, start: segment.start - removed, end: segment.end - removed }
        : segment,
  );
}
