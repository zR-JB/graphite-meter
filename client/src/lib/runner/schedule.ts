/* ============================================================
 * The Graphite Meter — Phase timeline (schedule)
 * Pure, engine-agnostic construction of the run's phase segments.
 * Each enabled stage is preceded by its own self-contained warmup
 * (see the warmup contract in contract.ts). Shared by every engine
 * so the dummy and a real runner sequence phases identically.
 * ============================================================ */

import type { RunnerConfig, Phase, FlowDirection, PhaseActivity } from "./contract";

/** A measured stage a warmup window primes. */
export type StagePhase = Extract<Phase, "latency" | "download" | "upload" | "bidirectional">;

/** One phase window on the run timeline. Every segment carries the resolved
 *  `activity` of its stage: a warmup segment and the measured-stage segment that
 *  follows it carry the SAME `activity` object, so the backend opens one
 *  connection set across both (see the stage-lifecycle contract in contract.ts). */
export interface Segment {
  phase: Extract<Phase, "warmup" | "latency" | "download" | "upload" | "bidirectional">;
  start: number; // ms offset from run start
  end: number;
  activity: PhaseActivity; // what this segment exercises (lanes + loaded latency)
}

export interface Timeline {
  segments: Segment[];
  totalMs: number;
}

/** Resolve a stage's {@link PhaseActivity} from config — the SINGLE place the
 *  "is loaded latency active?" rule lives (the latency stage is on, or loaded
 *  pings are not suppressed when it is off). The backend never re-derives any of
 *  this; it reads only the activity handed to it. */
function activityFor(stage: StagePhase, config: RunnerConfig): PhaseActivity {
  const loadedLatency = config.stages.latency || !config.skipLoadedLatencyWhenStageOff;
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
    // The latency stage measures IDLE latency — never "loaded"; only transfer
    // stages carry concurrent (bufferbloat) pings.
    loadedLatency: stage === "latency" ? false : loadedLatency,
  };
}

/** Extend a stage's warmup so TCP slow-start can fill the BDP before the measured
 *  window opens. A fixed warmup is too short on a high-RTT/far path — the window
 *  opens mid-ramp, so the measured rate sits below the true line speed. ~10 RTTs
 *  covers slow-start for typical BDPs (parallel lanes fill faster still); the
 *  configured warmup is the floor (a LAN keeps it), capped so a satellite-grade RTT
 *  can't blow the run length. rttMs ≤ 0 (no probe) ⇒ the configured value. */
export function adaptiveWarmupMs(baseMs: number, rttMs: number): number {
  const SLOW_START_RTTS = 10;
  const CEIL_MS = 4000;
  const rtt = Number.isFinite(rttMs) && rttMs > 0 ? rttMs : 0; // NaN/garbage ⇒ floor
  return Math.min(CEIL_MS, Math.max(baseMs, Math.round(rtt * SLOW_START_RTTS)));
}

/** Build the full phase timeline for a run, skipping disabled stages. Each
 *  enabled stage owns a self-contained warmup that primes its own connection —
 *  no global initial warmup, so stages carry no cross-deps. Because every
 *  warmup is immediately followed by its stage's measurement, two warmups can
 *  never sit adjacent. */
export function buildSegments(config: RunnerConfig): Timeline {
  const segs: Segment[] = [];
  let cursor = 0;
  const push = (phase: Segment["phase"], ms: number, activity: PhaseActivity) => {
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
  stage(config.stages.bidirectional, "bidirectional", config.duration.bidirectionalMs);
  return { segments: segs, totalMs: cursor };
}

/** Apply a live change to the enabled stage set mid-run. Only FUTURE segments
 *  (those starting after the current elapsed) are rebuilt; the current and past
 *  phases are untouched, so toggling a not-yet-started stage off simply shortens
 *  the remaining timeline. `config` must already carry the updated `stages`. */
export function rebuildTail(
  segments: Segment[],
  elapsed: number,
  config: RunnerConfig,
): Timeline {
  // Keep every segment that has already started; rebuild the tail.
  const kept = segments.filter((s) => s.start <= elapsed);
  let cursor = kept.length ? kept[kept.length - 1].end : 0;

  const dur = config.duration;
  const w = dur.warmupMs;
  const tail: Segment[] = [];
  const pushStage = (on: boolean, phase: StagePhase, ms: number) => {
    // Skip disabled phases and ones whose measurement already started.
    if (!on || ms <= 0) return;
    if (kept.some((k) => k.phase === phase)) return;
    // If this stage's warmup is already running, REUSE its activity object so
    // the warmup→measure seam still shares one connection set (and one loaded-
    // latency decision) even though we rebuilt the tail under it; otherwise
    // resolve a fresh activity from the updated config.
    const keptWarmup = kept.find((k) => k.phase === "warmup" && k.activity.stage === phase);
    const activity = keptWarmup ? keptWarmup.activity : activityFor(phase, config);
    // Prepend this stage's own warmup — unless it is already running (kept),
    // in which case the measurement just follows it.
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
export function segmentAt(segments: Segment[], elapsed: number): Segment | undefined {
  return segments.find((s) => elapsed >= s.start && elapsed < s.end);
}
