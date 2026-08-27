/* The Graphite Meter phase timeline: pure, engine-agnostic construction of the run's segments. */

import type {
  RunnerConfig,
  Phase,
  FlowDirection,
  PhaseActivity,
} from "./contract";

/** A measured stage a warmup window primes. */
const STAGE_PHASES = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const;
export type StagePhase = (typeof STAGE_PHASES)[number];

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

/* Resolve a stage's {@link PhaseActivity} from config. */
function activityFor(stage: StagePhase, config: RunnerConfig): PhaseActivity {
  const loadedLatency =
    config.stages.latency || !config.skipLoadedLatencyWhenStageOff;
  const transfer: FlowDirection[] =
    stage === "download"
      ? ["down"]
      : stage === "upload"
        ? ["up"]
        : stage === "bidirectional"
          ? ["down", "up"]
          : []; // latency stage moves no bytes
  return {
    stage,
    transfer,
    // The latency stage measures IDLE latency; only transfer stages carry concurrent (bufferbloat) pings.
    loadedLatency: stage === "latency" ? false : loadedLatency,
  };
}

/** Slow-start covers a typical BDP within this many RTTs; parallel lanes fill it faster still. */
const SLOW_START_RTTS = 10;
/** Ceiling, so a satellite-grade RTT cannot blow up the run length. */
const WARMUP_CEIL_MS = 4000;

/* Warmup scales with RTT to prime TCP slow-start; `baseMs` is the floor, and invalid RTT keeps it. */
export function adaptiveWarmupMs(baseMs: number, rttMs: number): number {
  const rtt = Number.isFinite(rttMs) && rttMs > 0 ? rttMs : 0;
  return Math.min(
    WARMUP_CEIL_MS,
    Math.max(baseMs, Math.round(rtt * SLOW_START_RTTS)),
  );
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
  const stage = (on: boolean, phase: StagePhase, ms: number) => {
    if (!on || ms <= 0) return;
    const activity = activityFor(phase, config);
    if (w > 0) push("warmup", w, activity); // prime this stage's connection(s) first
    push(phase, ms, activity);
  };
  for (const phase of STAGE_PHASES)
    stage(config.stages[phase], phase, config.duration[`${phase}Ms`]);
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

  const dur = config.duration;
  const w = dur.warmupMs;
  const tail: Segment[] = [];
  const pushStage = (on: boolean, phase: StagePhase, ms: number) => {
    // Skip disabled phases and ones whose measurement already started.
    if (!on || ms <= 0) return;
    if (kept.some((k) => k.phase === phase)) return;
    // A running warmup retains its activity object, so measurement reuses its connections and latency policy.
    const keptWarmup = kept.find(
      (k) => k.phase === "warmup" && k.activity.stage === phase,
    );
    const activity = keptWarmup
      ? keptWarmup.activity
      : activityFor(phase, config);
    // Prepend this stage's own warmup unless it is already running.
    if (w > 0 && !keptWarmup) {
      tail.push({ phase: "warmup", start: cursor, end: cursor + w, activity });
      cursor += w;
    }
    tail.push({ phase, start: cursor, end: cursor + ms, activity });
    cursor += ms;
  };
  for (const phase of STAGE_PHASES)
    pushStage(config.stages[phase], phase, dur[`${phase}Ms`]);

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
