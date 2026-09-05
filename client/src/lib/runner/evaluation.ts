/* Graphite Meter evaluation core: engine-agnostic accumulation and reduction. */

import type {
  RunnerConfig,
  ThroughputResult,
  LatencyResult,
  BufferbloatGrade,
  FlowDirection,
  StageLatencySummary,
} from "./contract";
import type { StagePhase } from "./schedule";
import {
  transferConfidence,
  latencyConfidence,
  LATENCY_CONFIDENCE_WINDOW_MS,
  bandForState,
  isStillStable,
  TRANSFER_CONFIDENCE_BUCKETS,
  type ConfidenceScore,
  type LatencyConfidenceScore,
} from "./adaptive";
import { median } from "./stats";
import { LatencyAccumulator } from "./latencySummary";
import { FixedRateBuckets, PairedRateBuckets } from "./controlBuckets";

export const MIN_PARTIAL_TRANSFER_EVIDENCE_MS = 800;
export const MIN_PARTIAL_LATENCY_OUTCOMES = 3;
const MIN_PARTIAL_LATENCY_SUCCESSES = 1;

interface PhaseAccum {
  bytes: number;
  evidenceMs: number;
  bytesBeforeLatest: number;
  evidenceBeforeLatestMs: number;
  peakBytesPerSec: number;
  stabilityBuckets: FixedRateBuckets;
  serverAuthoritative: boolean;
}

interface StableBaseline {
  bytes: number;
  evidenceMs: number;
}

type TransferLane =
  "download" | "upload" | "bidirectional-down" | "bidirectional-up";

interface LaneState extends PhaseAccum {
  stableStartMs: number;
  stableBaseline: StableBaseline | null;
  finalScore: number;
}

type LaneStates = Record<TransferLane, LaneState>;

const emptyLaneState = (): LaneState => ({
  bytes: 0,
  evidenceMs: 0,
  bytesBeforeLatest: 0,
  evidenceBeforeLatestMs: 0,
  peakBytesPerSec: 0,
  stabilityBuckets: new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
  serverAuthoritative: false,
  stableStartMs: -1,
  stableBaseline: null,
  finalScore: 0,
});

const emptyLaneStates = (): LaneStates => ({
  download: emptyLaneState(),
  upload: emptyLaneState(),
  "bidirectional-down": emptyLaneState(),
  "bidirectional-up": emptyLaneState(),
});

export class RunAccumulator {
  // ---- whole-run result bookkeeping ----
  #lanes: LaneStates = emptyLaneStates();
  #latency = this.#emptyLatency();

  #emptyLatency(): Record<StagePhase, LatencyAccumulator> {
    return {
      latency: new LatencyAccumulator(),
      download: new LatencyAccumulator(),
      upload: new LatencyAccumulator(),
      bidirectional: new LatencyAccumulator(),
    };
  }

  #phaseBuckets = {
    download: new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
    upload: new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
    bidirectional: new PairedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
  };
  #phaseLatency: { tMs: number; rttMs: number | null }[] = [];

  #latStableStartIndex = -1;
  #biStable = false;
  #latFinalScore = 0;

  #latEarlyStopStart = -1;
  #latEarlyStopCandidateStart = -1;

  reset(): void {
    this.#lanes = emptyLaneStates();
    this.#latency = this.#emptyLatency();
    this.#latStableStartIndex = -1;
    this.#biStable = false;
    this.#latFinalScore = 0;
    this.#latEarlyStopStart = -1;
    this.#latEarlyStopCandidateStart = -1;
    this.beginPhase();
  }

  beginPhase(): void {
    for (const buckets of Object.values(this.#phaseBuckets)) buckets.reset();
    this.#phaseLatency = [];
  }

  /* ================= SAMPLE INGEST ================= */

  /** Record a transfer sample and feed its phase confidence window. */
  pushThroughput(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    bytesDelta: number,
    durationSec: number,
    serverAuthoritative = false,
    observedAtMs?: number,
  ): void {
    if (durationSec <= 0) return;
    const accum = this.#lane(phase, dir);
    const durationMs = durationSec * 1_000;
    const bytes = Math.max(0, bytesDelta);
    const rate = bytes / durationSec;
    accum.bytesBeforeLatest = accum.bytes;
    accum.evidenceBeforeLatestMs = accum.evidenceMs;
    accum.bytes += bytes;
    accum.evidenceMs += durationMs;
    accum.peakBytesPerSec = Math.max(accum.peakBytesPerSec, rate);
    accum.stabilityBuckets.observe(bytes, durationMs);
    accum.serverAuthoritative ||= serverAuthoritative;
    if (phase === "bidirectional")
      this.#phaseBuckets.bidirectional.observe(
        dir,
        bytes,
        durationMs,
        observedAtMs,
      );
    else this.#phaseBuckets[phase].observe(bytes, durationMs);
  }

  /** Account a server-clock recovery gap only in exact final byte/time reduction. */
  recordRecoveryGap(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    durationSec: number,
  ): void {
    if (durationSec <= 0) return;
    const accum = this.#lane(phase, dir);
    const durationMs = durationSec * 1_000;
    accum.bytesBeforeLatest = accum.bytes;
    accum.evidenceBeforeLatestMs = accum.evidenceMs;
    accum.evidenceMs += durationMs;
    accum.stabilityBuckets.observe(0, durationMs);
    accum.serverAuthoritative = true;
  }

  /** Account replacement bytes without creating a control or presentation sample. */
  recordRecoveryBytes(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    bytes: number,
  ): void {
    if (bytes <= 0) return;
    const accum = this.#lane(phase, dir);
    accum.bytes += bytes;
    accum.serverAuthoritative = true;
  }

  #lane(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
  ): LaneState {
    const key: TransferLane =
      phase === "bidirectional" ? `bidirectional-${dir}` : phase;
    return this.#lanes[key];
  }

  #phaseLanes(phase: StagePhase): LaneState[] {
    if (phase === "latency") return [];
    return phase === "bidirectional"
      ? [this.#lanes["bidirectional-down"], this.#lanes["bidirectional-up"]]
      : [this.#lanes[phase]];
  }

  /** Attribute every outcome to its measured stage and continuity segment. */
  pushLatency(
    phase: StagePhase,
    rttMs: number,
    timedOut: boolean,
    tMs = this.#phaseLatency.at(-1)?.tMs ?? 0,
    continuityId = 0,
    rttEligible = true,
    reflectorHandlingMs?: number,
  ): void {
    this.#latency[phase].observe(
      rttMs,
      timedOut,
      continuityId,
      rttEligible,
      reflectorHandlingMs,
    );
    if (phase === "latency" && rttEligible) {
      this.#phaseLatency.push({ tMs, rttMs: timedOut ? null : rttMs });
      const cutoff = tMs - LATENCY_CONFIDENCE_WINDOW_MS;
      while (this.#phaseLatency[0]?.tMs <= cutoff) this.#phaseLatency.shift();
    }
  }

  interruptLatency(
    phase: StagePhase,
    count: number,
    reason: "unresolved" | "send-failed",
  ): void {
    this.#latency[phase].interrupt(count, reason);
  }

  markLatencyAccountingIncomplete(phase: StagePhase): void {
    this.#latency[phase].markAccountingIncomplete();
  }

  latencySummary(phase: StagePhase): StageLatencySummary | null {
    return this.#latency[phase].snapshot();
  }

  latencySummaries(): Record<StagePhase, StageLatencySummary | null> {
    return {
      latency: this.latencySummary("latency"),
      download: this.latencySummary("download"),
      upload: this.latencySummary("upload"),
      bidirectional: this.latencySummary("bidirectional"),
    };
  }

  /* ================= STABILITY ================= */

  /** Return the active phase's confidence score. */
  confidence(phase: StagePhase): ConfidenceScore | LatencyConfidenceScore {
    if (phase === "latency") return latencyConfidence(this.#phaseLatency);
    return transferConfidence([...this.#phaseBuckets[phase].rates]);
  }

  resetPhaseStability(phase: StagePhase): void {
    if (phase === "latency") this.#phaseLatency = [];
    else this.#phaseBuckets[phase].reset();
    if (phase === "bidirectional") {
      this.#biStable = false;
    } else if (phase === "latency") this.#latStableStartIndex = -1;
    for (const lane of this.#phaseLanes(phase)) {
      lane.stableStartMs = -1;
      lane.stableBaseline = null;
    }
  }

  /** Update the trailing stable run from this tick's score. */
  trackStableRun(
    phase: StagePhase,
    score: number,
    cfg: RunnerConfig["adaptive"],
  ): boolean {
    if (phase === "latency") return this.#trackLatencyStableRun(score, cfg);
    const lanes = this.#phaseLanes(phase);
    const wasStable =
      phase === "bidirectional" ? this.#biStable : lanes[0].stableStartMs >= 0;
    const nowStable = isStillStable(wasStable, score, cfg);
    for (const lane of lanes) lane.finalScore = score;
    if (nowStable && !wasStable) {
      for (const lane of lanes) {
        lane.stableStartMs =
          lane.evidenceMs > 0 ? lane.evidenceBeforeLatestMs : -1;
        lane.stableBaseline = {
          bytes: lane.bytesBeforeLatest,
          evidenceMs: lane.evidenceBeforeLatestMs,
        };
      }
    } else if (!nowStable && wasStable) {
      for (const lane of lanes) {
        lane.stableStartMs = -1;
        lane.stableBaseline = null;
      }
    }
    if (phase === "bidirectional") this.#biStable = nowStable;
    return nowStable;
  }

  #trackLatencyStableRun(
    score: number,
    cfg: RunnerConfig["adaptive"],
  ): boolean {
    const wasStable = this.#latStableStartIndex >= 0;
    const nowStable = isStillStable(wasStable, score, cfg);
    this.#latFinalScore = score;
    if (nowStable && !wasStable)
      this.#latStableStartIndex = Math.max(
        0,
        this.#latency.latency.rtts.length - 1,
      );
    else if (!nowStable && wasStable) this.#latStableStartIndex = -1;
    return nowStable;
  }

  armLatencyEarlyStop(): void {
    if (this.#latEarlyStopCandidateStart < 0)
      this.#latEarlyStopCandidateStart = Math.max(
        0,
        this.#latency.latency.rtts.length - 1,
      );
  }

  cancelLatencyEarlyStop(): void {
    this.#latEarlyStopCandidateStart = -1;
  }

  confirmLatencyEarlyStop(): void {
    if (this.#latEarlyStopCandidateStart >= 0)
      this.#latEarlyStopStart = this.#latEarlyStopCandidateStart;
    this.#latEarlyStopCandidateStart = -1;
  }

  /* ================= RESULT REDUCTION ================= */

  /** Reduce a one-way transfer phase to effective bytes over represented time. */
  throughputResult(
    phase: "download" | "upload",
    useStableWindow: boolean,
  ): ThroughputResult {
    return this.#reduceLane(this.#lanes[phase], phase, useStableWindow);
  }

  /** Reduce a failed transfer after it has met the minimum evidence floor. */
  partialThroughputResult(
    phase: "download" | "upload",
  ): ThroughputResult | null {
    const lane = this.#lanes[phase];
    return lane.evidenceMs >= MIN_PARTIAL_TRANSFER_EVIDENCE_MS
      ? this.#reduceLane(lane, phase, false, 0, -1)
      : null;
  }

  /** Reduce both bidirectional lanes with the common effective-rate reducer. */
  bidirectionalResult(useStableWindow: boolean): {
    down: ThroughputResult;
    up: ThroughputResult;
  } {
    return {
      down: this.#reduceLane(
        this.#lanes["bidirectional-down"],
        "bidirectional",
        useStableWindow,
      ),
      up: this.#reduceLane(
        this.#lanes["bidirectional-up"],
        "bidirectional",
        useStableWindow,
      ),
    };
  }

  /** Reduce bidirectional lanes independently for a partial stage. */
  partialBidirectionalResult(): {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
  } {
    const reduce = (lane: LaneState): ThroughputResult | null =>
      lane.evidenceMs >= MIN_PARTIAL_TRANSFER_EVIDENCE_MS
        ? this.#reduceLane(lane, "bidirectional", false, 0, -1)
        : null;
    return {
      down: reduce(this.#lanes["bidirectional-down"]),
      up: reduce(this.#lanes["bidirectional-up"]),
    };
  }

  /** Resolve the adaptive stable window used by the latency headline. */
  #windowStart(
    stableStart: number,
    earlyStopStart: number,
    adaptiveEnabled: boolean,
    arrLen: number,
  ): number {
    if (!adaptiveEnabled || stableStart < 0 || stableStart >= arrLen) return -1;
    return earlyStopStart >= 0 && earlyStopStart < arrLen
      ? stableStart <= earlyStopStart
        ? earlyStopStart
        : -1
      : stableStart;
  }

  /** Shared effective-throughput reducer for every transfer direction. */
  #reduceLane(
    lane: LaneState,
    phase: StagePhase,
    useStableWindow: boolean,
    finalScore = lane.finalScore,
    stableStart = lane.stableStartMs,
  ): ThroughputResult {
    const band = bandForState(stableStart >= 0, finalScore);
    if (lane.evidenceMs <= 0) {
      return {
        peakBytesPerSec: 0,
        stabilityPct: 0,
        totalBytes: lane.bytes,
        reportedBytesPerSec: 0,
        fullAverageBytesPerSec: 0,
        method: "full-average",
        stabilityScore: finalScore,
        band,
        probeTimeoutPct: this.#latency[phase].probeTimeoutPct,
        serverAuthoritative: lane.serverAuthoritative || undefined,
      };
    }
    const full = lane.bytes / (lane.evidenceMs / 1_000);
    let reported = full;
    let hasStableEvidence = false;
    if (useStableWindow && lane.stableStartMs >= 0 && lane.stableBaseline) {
      const evidenceMs = lane.evidenceMs - lane.stableBaseline.evidenceMs;
      const bytes = lane.bytes - lane.stableBaseline.bytes;
      if (evidenceMs > 0) {
        reported = bytes / (evidenceMs / 1_000);
        hasStableEvidence = true;
      }
    }
    const stability = transferConfidence([...lane.stabilityBuckets.rates]);
    const descriptiveStability =
      stability.sampleCount >= 2
        ? Math.max(0, Math.min(1, 1 - stability.varianceRatio))
        : 0;

    return {
      peakBytesPerSec: lane.peakBytesPerSec,
      stabilityPct: descriptiveStability * 100,
      totalBytes: lane.bytes,
      reportedBytesPerSec: reported,
      fullAverageBytesPerSec: full,
      method: hasStableEvidence ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band,
      probeTimeoutPct: this.#latency[phase].probeTimeoutPct,
      serverAuthoritative: lane.serverAuthoritative || undefined,
    };
  }

  /** Reduce the measured idle population; absent RTT evidence has no result. */
  latencyResult(
    cfg: RunnerConfig,
    useAdaptive = cfg.adaptive.enabled,
  ): LatencyResult | null {
    const stableStart = this.#latStableStartIndex;
    const earlyStopStart = this.#latEarlyStopStart;
    const finalScore = this.#latFinalScore;
    const summary = this.latencySummary("latency");
    const idle = this.#latency.latency.rtts;
    if (!summary || !idle.length) return null;
    // The headline may use the stable window; descriptors always cover the full unloaded stage.
    const windowStart = this.#windowStart(
      stableStart,
      earlyStopStart,
      useAdaptive,
      idle.length,
    );
    const useWindow = windowStart >= 0;
    const idleWindow = useWindow ? idle.slice(windowStart) : idle;
    const idleMs = median(idleWindow);
    return {
      idleMs,
      minMs: summary.minMs,
      p50Ms: summary.p50Ms,
      p95Ms: summary.p95Ms,
      jitterMs: summary.jitterMs,
      probeTimeoutPct: this.#latency.latency.probeTimeoutPct,
      reportedMs: idleMs,
      method: useWindow ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band: bandForState(stableStart >= 0, finalScore),
    };
  }

  /** Latency evidence is usable only when enough outcomes include a success. */
  partialLatencyResult(cfg: RunnerConfig): LatencyResult | null {
    if (
      this.#latency.latency.count < MIN_PARTIAL_LATENCY_OUTCOMES ||
      this.#latency.latency.rtts.length < MIN_PARTIAL_LATENCY_SUCCESSES
    )
      return null;
    return this.latencyResult(cfg, false);
  }

  /** Bufferbloat grade from authoritative idle-vs-loaded RTT evidence. */
  bufferbloatGrade(): BufferbloatGrade | null {
    const loaded = ["download", "upload", "bidirectional"] as const;
    const medians = loaded.flatMap((phase) => {
      const rtts = this.#latency[phase].rtts;
      return rtts.length ? [median(rtts)] : [];
    });
    if (!this.#latency.latency.rtts.length || !medians.length) return null;
    const idleMs = median(this.#latency.latency.rtts);
    const loadedMs = Math.max(...medians);
    const increaseMs = Math.max(0, loadedMs - idleMs);
    let grade: BufferbloatGrade["grade"];
    if (increaseMs <= 5) grade = "A";
    else if (increaseMs <= 30) grade = "B";
    else if (increaseMs <= 60) grade = "C";
    else if (increaseMs <= 200) grade = "D";
    else grade = "F";
    return { grade, idleMs, loadedMs, increaseMs };
  }
}
