/* The Graphite Meter phase timeline: pure, engine-agnostic construction of the run's segments. */

import type {
  RunnerConfig,
  Phase,
  FlowDirection,
  PhaseActivity,
  TransportRole,
} from "./contract";

export const STAGES = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const satisfies readonly TransportRole[];

const TRANSFER: Record<TransportRole, readonly FlowDirection[]> = {
  latency: [],
  download: ["down"],
  upload: ["up"],
  bidirectional: ["down", "up"],
};

/** A stage runs only when it is on and has time; 0 ms skips it. */
export const planned = (
  config: Pick<RunnerConfig, "stages" | "duration">,
  stage: TransportRole,
): boolean => config.stages[stage] && config.duration[`${stage}Ms`] > 0;

/** Warmup and measurement share one activity; only transfer stages carry loaded-latency pings. */
export const activityFor = (
  stage: TransportRole,
  config: RunnerConfig,
): PhaseActivity => ({
  stage,
  transfer: [...TRANSFER[stage]],
  loadedLatency:
    stage !== "latency" &&
    (planned(config, "latency") || !config.skipLoadedLatencyWhenStageOff),
});

export const plannedActivities = (config: RunnerConfig): PhaseActivity[] =>
  STAGES.filter((stage) => planned(config, stage)).map((stage) =>
    activityFor(stage, config),
  );

/* Each segment carries the stage activity shared by its warmup and measured window. */
export interface Segment {
  phase: Extract<
    Phase,
    "warmup" | "latency" | "download" | "upload" | "bidirectional"
  >;
  start: number; // ms offset from run start
  end: number;
  activity: PhaseActivity; // what this segment exercises (lanes + loaded latency)
}

interface Timeline {
  segments: Segment[];
  totalMs: number;
}

/** Slow-start covers a typical BDP within this many RTTs; parallel lanes fill it faster still. */
const SLOW_START_RTTS = 10;
/** Ceiling, so a satellite-grade RTT cannot blow up the run length. */
const WARMUP_CEIL_MS = 4000;

/** The run's plan: warmup scales with RTT to prime TCP slow-start; an unknown RTT is Infinity. */
export function adaptWarmup(config: RunnerConfig, rttMs: number): RunnerConfig {
  const scaled = Math.round((rttMs > 0 ? rttMs : 0) * SLOW_START_RTTS);
  const warmupMs = Math.min(
    WARMUP_CEIL_MS,
    Math.max(config.duration.warmupMs, scaled),
  );
  return { ...config, duration: { ...config.duration, warmupMs } };
}

/* Every warmup is immediately followed by its stage's measurement, so two warmups never sit adjacent. */
export function buildSegments(config: RunnerConfig): Timeline {
  const segs: Segment[] = [];
  let cursor = 0;
  const push = (
    phase: Segment["phase"],
    ms: number,
    activity: PhaseActivity,
  ) => {
    if (ms <= 0) return;
    segs.push({ phase, start: cursor, end: cursor + ms, activity });
    cursor += ms;
  };
  const w = config.duration.warmupMs;
  for (const activity of plannedActivities(config)) {
    if (w > 0) push("warmup", w, activity); // prime this stage's connection(s) first
    push(activity.stage, config.duration[`${activity.stage}Ms`], activity);
  }
  return { segments: segs, totalMs: cursor };
}

/* Rebuild the unfinished timeline after a safe live config change. */
export function reconfigureTimeline(
  segments: Segment[],
  elapsed: number,
  config: RunnerConfig,
): Timeline {
  const active = segmentAt(segments, elapsed);
  const kept = active
    ? segments.filter((s) => s.start < active.start)
    : segments.filter((s) => s.end <= elapsed);

  if (active) {
    const duration =
      active.phase === "warmup"
        ? config.duration.warmupMs
        : config.duration[`${active.phase}Ms`];
    kept.push({
      ...active,
      end: Math.max(elapsed, active.start + duration),
    });
  }
  let cursor = kept.length ? kept[kept.length - 1].end : 0;

  const w = config.duration.warmupMs;
  const tail: Segment[] = [];
  for (const next of plannedActivities(config)) {
    const phase = next.stage;
    // A stage whose measurement already started stays as kept.
    if (kept.some((k) => k.phase === phase)) continue;
    // A running warmup retains its activity object, so measurement reuses its connections and latency policy.
    const keptWarmup = kept.find(
      (k) => k.phase === "warmup" && k.activity.stage === phase,
    );
    const activity = keptWarmup?.activity ?? next;
    if (w > 0 && !keptWarmup) {
      tail.push({ phase: "warmup", start: cursor, end: cursor + w, activity });
      cursor += w;
    }
    const ms = config.duration[`${phase}Ms`];
    tail.push({ phase, start: cursor, end: cursor + ms, activity });
    cursor += ms;
  }

  return { segments: [...kept, ...tail], totalMs: cursor };
}

/** The segment covering `elapsed`, or undefined past the end. */
export function segmentAt(
  segments: Segment[],
  elapsed: number,
): Segment | undefined {
  return segments.find((s) => elapsed >= s.start && elapsed < s.end);
}

/* Close at a measured boundary, shift the untouched tail by removed budget, and fabricate no measured time. */
export function truncateSegmentAt(
  segments: Segment[],
  active: Segment,
  elapsed: number,
): Timeline {
  const boundary = Math.min(active.end, Math.max(active.start, elapsed));
  const removed = active.end - boundary;
  const next = segments.map((segment) => {
    if (segment === active) return { ...segment, end: boundary };
    if (segment.start >= active.end)
      return {
        ...segment,
        start: segment.start - removed,
        end: segment.end - removed,
      };
    return segment;
  });
  return { segments: next, totalMs: next.at(-1)?.end ?? 0 };
}
