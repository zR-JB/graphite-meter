/* ============================================================
 * The Graphite Meter — Phase timeline (schedule)
 * Pure, engine-agnostic construction of the run's phase segments.
 * Each enabled stage is preceded by its own self-contained warmup
 * (see the warmup contract in contract.ts). Shared by every engine
 * so the dummy and a real runner sequence phases identically.
 * ============================================================ */

import type { RunnerConfig, Phase } from "./contract";

/** A measured stage a warmup window primes. */
export type StagePhase = Extract<Phase, "latency" | "download" | "upload" | "bidirectional">;

/** One phase window on the run timeline. `warmupFor` is set only on warmup
 *  segments — backend-only metadata naming the stage the warmup primes. */
export interface Segment {
  phase: Extract<Phase, "warmup" | "latency" | "download" | "upload" | "bidirectional">;
  start: number; // ms offset from run start
  end: number;
  warmupFor?: StagePhase; // set only on warmup segments
}

export interface Timeline {
  segments: Segment[];
  totalMs: number;
}

/** Build the full phase timeline for a run, skipping disabled stages. Each
 *  enabled stage owns a self-contained warmup that primes its own connection —
 *  no global initial warmup, so stages carry no cross-deps. Because every
 *  warmup is immediately followed by its stage's measurement, two warmups can
 *  never sit adjacent. */
export function buildSegments(config: RunnerConfig): Timeline {
  const segs: Segment[] = [];
  let cursor = 0;
  const push = (phase: Segment["phase"], ms: number, warmupFor?: StagePhase) => {
    if (ms <= 0) return;
    segs.push({ phase, start: cursor, end: cursor + ms, warmupFor });
    cursor += ms;
  };
  const w = config.duration.warmupMs;
  const stage = (on: boolean, phase: StagePhase, ms: number) => {
    if (!on || ms <= 0) return;
    if (w > 0) push("warmup", w, phase); // warm this stage's connection first
    push(phase, ms);
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
    // Prepend this stage's own warmup — unless it is already running (kept),
    // in which case the measurement just follows it.
    const warmupRunning = kept.some(
      (k) => k.phase === "warmup" && k.warmupFor === phase,
    );
    if (w > 0 && !warmupRunning) {
      tail.push({ phase: "warmup", start: cursor, end: cursor + w, warmupFor: phase });
      cursor += w;
    }
    tail.push({ phase, start: cursor, end: cursor + ms });
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
