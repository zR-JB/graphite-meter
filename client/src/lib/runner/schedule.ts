/* ============================================================
 * The Graphite Meter: phase timeline (schedule)
 * Pure, engine-agnostic construction of the run's phase segments.
 * Each enabled stage is preceded by its own self-contained warmup
 * (see the warmup contract in contract.ts). Shared by every engine
 * so the dummy and a real runner sequence phases identically.
 * ============================================================ */

import type {
  RunnerConfig,
  Phase,
  FlowDirection,
  PhaseActivity,
} from "./contract";

/** A measured stage a warmup window primes. */
export type StagePhase = Extract<
  Phase,
  "latency" | "download" | "upload" | "bidirectional"
>;

/** One phase window on the run timeline. Every segment carries the resolved
 *  `activity` of its stage: a warmup segment and the measured-stage segment that
 *  follows it carry the SAME `activity` object, so the backend opens one
 *  connection set across both (see the stage-lifecycle contract in contract.ts). */
export interface Segment {
  phase: Extract<
    Phase,
    "warmup" | "latency" | "download" | "upload" | "bidirectional"
  >;
  start: number; // ms offset from run start
  end: number;
  activity: PhaseActivity; // what this segment exercises (lanes + loaded latency)
}

export interface Timeline {
  segments: Segment[];
  totalMs: number;
}

/** Resolve a stage's {@link PhaseActivity} from config. The SINGLE place the
 *  "is loaded latency active?" rule lives: the latency stage is on, or loaded
 *  pings are not suppressed while it is off. The backend re-derives none of it,
 *  reading only the activity handed to it. */
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
    // The latency stage measures IDLE latency; only transfer stages carry
    // concurrent (bufferbloat) pings.
    loadedLatency: stage === "latency" ? false : loadedLatency,
  };
}

/** Slow-start covers a typical BDP within this many RTTs; parallel lanes fill
 *  it faster still. */
const SLOW_START_RTTS = 10;
/** Ceiling, so a satellite-grade RTT cannot blow up the run length. */
const WARMUP_CEIL_MS = 4000;

/** A warmup long enough for TCP slow-start to fill the BDP, so the measured
 *  window opens at line speed instead of mid-ramp. The configured `baseMs` is
 *  the floor, which a LAN keeps. rttMs ≤ 0 or non-finite yields that floor. */
export function adaptiveWarmupMs(baseMs: number, rttMs: number): number {
  const rtt = Number.isFinite(rttMs) && rttMs > 0 ? rttMs : 0;
  return Math.min(
    WARMUP_CEIL_MS,
    Math.max(baseMs, Math.round(rtt * SLOW_START_RTTS)),
  );
}

/** Build the run's phase timeline, skipping disabled stages. Each enabled stage
 *  owns a self-contained warmup that primes its own connection, so stages carry
 *  no cross-deps. Every warmup is immediately followed by its stage's
 *  measurement, so two warmups never sit adjacent. */
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
  stage(config.stages.latency, "latency", config.duration.latencyMs);
  stage(config.stages.download, "download", config.duration.downloadMs);
  stage(config.stages.upload, "upload", config.duration.uploadMs);
  // Bidirectional (concurrent down+up) runs last, with its own warmup.
  stage(
    config.stages.bidirectional,
    "bidirectional",
    config.duration.bidirectionalMs,
  );
  return { segments: segs, totalMs: cursor };
}

/** Rebuild the unfinished timeline after a safe live config change. Past
 * segments keep their actual boundaries, the active segment adopts its new
 * duration from its original start, and future stages are rebuilt normally. */
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
    // A running warmup keeps its activity object, so the warmup→measure seam
    // still shares one connection set and one loaded-latency decision.
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
  pushStage(config.stages.latency, "latency", dur.latencyMs);
  pushStage(config.stages.download, "download", dur.downloadMs);
  pushStage(config.stages.upload, "upload", dur.uploadMs);
  pushStage(config.stages.bidirectional, "bidirectional", dur.bidirectionalMs);

  return { segments: [...kept, ...tail], totalMs: cursor };
}

/** The segment covering `elapsed`, or undefined past the end. */
export function segmentAt(
  segments: Segment[],
  elapsed: number,
): Segment | undefined {
  return segments.find((s) => elapsed >= s.start && elapsed < s.end);
}

/** Close the active segment at a real measured boundary and shift the untouched
 * tail earlier by exactly the removed budget. No elapsed time is fabricated. */
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
