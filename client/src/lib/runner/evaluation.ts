/* ============================================================
 * The Graphite Meter: evaluation core (RunAccumulator)
 * Engine-agnostic accumulation and reduction of raw samples:
 * per-phase accumulation, the adaptive confidence windows, the
 * hysteretic stable-run tracker, and the final result reducers.
 * No simulation and no network I/O, so identical samples from the
 * dummy or from a real runner yield identical results.
 * ============================================================ */

import type {
  RunnerConfig,
  ThroughputResult,
  LatencyResult,
  BufferbloatGrade,
  FlowDirection,
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
import { median, percentile, meanAbsDeviation } from "./stats";
import { FixedRateBuckets, PairedRateBuckets } from "./controlBuckets";

export const MIN_PARTIAL_TRANSFER_EVIDENCE_MS = 800;
export const MIN_PARTIAL_LATENCY_OUTCOMES = 3;
export const MIN_PARTIAL_LATENCY_SUCCESSES = 1;

/** Per-transfer-phase sample bookkeeping for the final result. */
interface PhaseAccum {
  bytes: number;
  evidenceMs: number;
  bytesBeforeLatest: number;
  evidenceBeforeLatestMs: number;
  peakBytesPerSec: number;
  stabilityBuckets: FixedRateBuckets;
  serverAuthoritative: boolean;
}

const emptyPhaseAccum = (): PhaseAccum => ({
  bytes: 0,
  evidenceMs: 0,
  bytesBeforeLatest: 0,
  evidenceBeforeLatestMs: 0,
  peakBytesPerSec: 0,
  stabilityBuckets: new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
  serverAuthoritative: false,
});

interface StableBaseline {
  bytes: number;
  evidenceMs: number;
}

export class RunAccumulator {
  // ---- whole-run result bookkeeping ----
  #dl: PhaseAccum = emptyPhaseAccum();
  #ul: PhaseAccum = emptyPhaseAccum();
  // Bidirectional carries TWO concurrent lanes (down + up), each reduced with
  // the same throughput reducer as a normal transfer phase.
  #biDown: PhaseAccum = emptyPhaseAccum();
  #biUp: PhaseAccum = emptyPhaseAccum();
  #idleRtts: number[] = [];
  #loadedRtts: number[] = [];
  #allRtts: number[] = [];
  #pingsTotal = 0;
  #pingsLost = 0;
  // Under-load ping timeouts are a quality signal, not inferred TCP loss.
  #loadedPings = 0;
  #loadedPingsLost = 0;

  // ---- per-phase confidence windows (reset each measured phase) ----
  #phaseDownBuckets = new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS);
  #phaseUpBuckets = new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS);
  #phaseBidirectionalBuckets = new PairedRateBuckets(
    TRANSFER_CONFIDENCE_BUCKETS,
  );
  #phaseLatency: { tMs: number; rttMs: number | null }[] = [];

  // ---- trailing contiguous stable-run trackers ----
  // Transfer boundaries use their lane's exact evidence clock; latency uses
  // its outcome index. Every boundary is -1 while the phase is not stable.
  #dlStableStartMs = -1;
  #ulStableStartMs = -1;
  #latStableStartIndex = -1;
  // Bidi tracks ONE stable run over the combined-rate confidence window: the
  // phase has a single early-stop signal even though it reports two lanes.
  #biStable = false;
  #biStableStartDownMs = -1;
  #biStableStartUpMs = -1;
  #dlStableBaseline: StableBaseline | null = null;
  #ulStableBaseline: StableBaseline | null = null;
  #biStableBaselineDown: StableBaseline | null = null;
  #biStableBaselineUp: StableBaseline | null = null;
  #dlFinalScore = 0;
  #ulFinalScore = 0;
  #latFinalScore = 0;
  #biFinalScore = 0;

  // Latency uses a candidate-to-end median rule. Throughput result selection is
  // independent of the confirmation candidate and reads the final stable run.
  #latEarlyStopStart = -1;
  #latEarlyStopCandidateStart = -1;

  /** Reset all run state. Call at the start of each run. */
  reset(): void {
    this.#dl = emptyPhaseAccum();
    this.#ul = emptyPhaseAccum();
    this.#biDown = emptyPhaseAccum();
    this.#biUp = emptyPhaseAccum();
    this.#idleRtts = [];
    this.#loadedRtts = [];
    this.#allRtts = [];
    this.#pingsTotal = 0;
    this.#pingsLost = 0;
    this.#loadedPings = 0;
    this.#loadedPingsLost = 0;
    this.#dlStableStartMs = -1;
    this.#ulStableStartMs = -1;
    this.#latStableStartIndex = -1;
    this.#biStable = false;
    this.#biStableStartDownMs = -1;
    this.#biStableStartUpMs = -1;
    this.#dlStableBaseline = null;
    this.#ulStableBaseline = null;
    this.#biStableBaselineDown = null;
    this.#biStableBaselineUp = null;
    this.#dlFinalScore = 0;
    this.#ulFinalScore = 0;
    this.#latFinalScore = 0;
    this.#biFinalScore = 0;
    this.#latEarlyStopStart = -1;
    this.#latEarlyStopCandidateStart = -1;
    this.beginPhase();
  }

  /** Reset the per-phase confidence windows when a measured phase begins. */
  beginPhase(): void {
    this.#phaseDownBuckets.reset();
    this.#phaseUpBuckets.reset();
    this.#phaseBidirectionalBuckets.reset();
    this.#phaseLatency = [];
  }

  /* ================= SAMPLE INGEST ================= */

  /** Record a transfer sample: instantaneous bytes/sec plus exact bytes and
   *  duration for time-weighted reduction, tagged with direction. Bidirectional
   *  routes the sample to its lane and feeds the COMBINED rate (this lane plus
   *  the other lane's latest) into the single confidence window. */
  pushThroughput(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    _bytesPerSec: number,
    bytesDelta: number,
    durationSec: number,
    serverAuthoritative = false,
  ): void {
    if (durationSec <= 0) return;
    const accum = this.#transferAccum(phase, dir);
    const seconds = durationSec;
    const durationMs = seconds * 1_000;
    const bytes = Math.max(0, bytesDelta);
    const rate = bytes / seconds;
    accum.bytesBeforeLatest = accum.bytes;
    accum.evidenceBeforeLatestMs = accum.evidenceMs;
    accum.bytes += bytes;
    accum.evidenceMs += durationMs;
    accum.peakBytesPerSec = Math.max(accum.peakBytesPerSec, rate);
    accum.stabilityBuckets.observe(bytes, durationMs);
    accum.serverAuthoritative ||= serverAuthoritative;
    if (phase === "bidirectional")
      this.#phaseBidirectionalBuckets.observe(dir, bytes, durationMs);
    else
      (dir === "down" ? this.#phaseDownBuckets : this.#phaseUpBuckets).observe(
        bytes,
        durationMs,
      );
  }

  /** Account the time between upload IDs after their server clocks can no
   * longer represent one continuous interval. This is deliberately absent from
   * control buckets and presentation history: it affects only exact final
   * byte/time reduction. */
  recordRecoveryGap(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    durationSec: number,
  ): void {
    if (durationSec <= 0) return;
    const accum = this.#transferAccum(phase, dir);
    const durationMs = durationSec * 1_000;
    accum.bytesBeforeLatest = accum.bytes;
    accum.evidenceBeforeLatestMs = accum.evidenceMs;
    accum.evidenceMs += durationMs;
    accum.stabilityBuckets.observe(0, durationMs);
    accum.serverAuthoritative = true;
  }

  /** The first count from a replacement upload id has authoritative bytes but
   * no earlier replacement checkpoint for a rate interval. Keep those bytes
   * in the exact final numerator; the paired recovery-gap entry supplies the
   * represented time without creating a control or presentation sample. */
  recordRecoveryBytes(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    bytes: number,
  ): void {
    if (bytes <= 0) return;
    const accum = this.#transferAccum(phase, dir);
    accum.bytes += bytes;
    accum.serverAuthoritative = true;
  }

  #transferAccum(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
  ): PhaseAccum {
    return phase === "bidirectional"
      ? dir === "down"
        ? this.#biDown
        : this.#biUp
      : phase === "download"
        ? this.#dl
        : this.#ul;
  }

  /** Record a ping sample. Latency confidence uses unloaded RTTs + the loss
   *  count over the same window. */
  pushLatency(
    rttMs: number,
    underLoad: boolean,
    lost: boolean,
    tMs = this.#phaseLatency.at(-1)?.tMs ?? 0,
  ): void {
    this.#pingsTotal++;
    if (underLoad) this.#loadedPings++;
    if (lost) {
      this.#pingsLost++;
      if (underLoad) this.#loadedPingsLost++;
    } else {
      this.#allRtts.push(rttMs);
      if (underLoad) this.#loadedRtts.push(rttMs);
      else this.#idleRtts.push(rttMs);
    }
    if (!underLoad) {
      this.#phaseLatency.push({ tMs, rttMs: lost ? null : rttMs });
      const cutoff = tMs - LATENCY_CONFIDENCE_WINDOW_MS;
      while (this.#phaseLatency[0]?.tMs <= cutoff) this.#phaseLatency.shift();
    }
  }

  /* ================= STABILITY ================= */

  /** Stability for the active measured phase over its current confidence
   *  window: the single signal the pip, the early-finish decision, and the
   *  result selection all read. */
  confidence(phase: StagePhase): ConfidenceScore | LatencyConfidenceScore {
    if (phase === "latency") return latencyConfidence(this.#phaseLatency);
    if (phase === "download")
      return transferConfidence([...this.#phaseDownBuckets.rates]);
    if (phase === "upload")
      return transferConfidence([...this.#phaseUpBuckets.rates]);
    return transferConfidence([...this.#phaseBidirectionalBuckets.rates]);
  }

  /** A confirmed regime change or stall invalidates control history only. */
  resetPhaseStability(phase: StagePhase): void {
    if (phase === "download") this.#phaseDownBuckets.reset();
    if (phase === "upload") this.#phaseUpBuckets.reset();
    if (phase === "bidirectional") this.#phaseBidirectionalBuckets.reset();
    if (phase === "latency") this.#phaseLatency = [];
    if (phase === "download") this.#dlStableStartMs = -1;
    else if (phase === "upload") this.#ulStableStartMs = -1;
    else if (phase === "bidirectional") {
      this.#biStable = false;
      this.#biStableStartDownMs = -1;
      this.#biStableStartUpMs = -1;
    } else this.#latStableStartIndex = -1;
    if (phase === "download") this.#dlStableBaseline = null;
    else if (phase === "upload") this.#ulStableBaseline = null;
    else if (phase === "bidirectional") {
      this.#biStableBaselineDown = null;
      this.#biStableBaselineUp = null;
    }
  }

  /** Update the trailing stable run from this tick's score. Transfer lanes use
   *  evidence-time boundaries and latency uses an outcome index;
   *  `isStillStable` supplies the shared hysteresis. */
  trackStableRun(
    phase: StagePhase,
    score: number,
    cfg: RunnerConfig["adaptive"],
  ): boolean {
    let start: number;
    if (phase === "download") {
      this.#dlFinalScore = score;
      start = this.#dlStableStartMs;
    } else if (phase === "upload") {
      this.#ulFinalScore = score;
      start = this.#ulStableStartMs;
    } else if (phase === "bidirectional") {
      this.#biFinalScore = score;
      start = -1;
    } else {
      this.#latFinalScore = score;
      start = this.#latStableStartIndex;
    }
    const wasStable = phase === "bidirectional" ? this.#biStable : start >= 0;
    const nowStable = isStillStable(wasStable, score, cfg);
    if (nowStable && !wasStable) {
      if (phase === "bidirectional") {
        this.#biStableStartDownMs = this.#latestEvidenceStart(this.#biDown);
        this.#biStableStartUpMs = this.#latestEvidenceStart(this.#biUp);
        this.#biStableBaselineDown = this.#latestBaseline(this.#biDown);
        this.#biStableBaselineUp = this.#latestBaseline(this.#biUp);
      } else {
        start = this.#stableEvidenceStart(phase);
        if (phase === "download")
          this.#dlStableBaseline = this.#latestBaseline(this.#dl);
        else if (phase === "upload")
          this.#ulStableBaseline = this.#latestBaseline(this.#ul);
      }
    } else if (!nowStable && wasStable) {
      start = -1;
      if (phase === "bidirectional") {
        this.#biStableStartDownMs = -1;
        this.#biStableStartUpMs = -1;
        this.#biStableBaselineDown = null;
        this.#biStableBaselineUp = null;
      }
      if (phase === "download") this.#dlStableBaseline = null;
      if (phase === "upload") this.#ulStableBaseline = null;
    }

    if (phase === "download") this.#dlStableStartMs = start;
    else if (phase === "upload") this.#ulStableStartMs = start;
    else if (phase === "bidirectional") this.#biStable = nowStable;
    else this.#latStableStartIndex = start;

    return nowStable;
  }

  armLatencyEarlyStop(): void {
    if (this.#latEarlyStopCandidateStart < 0)
      this.#latEarlyStopCandidateStart = Math.max(0, this.#idleRtts.length - 1);
  }

  cancelLatencyEarlyStop(): void {
    this.#latEarlyStopCandidateStart = -1;
  }

  confirmLatencyEarlyStop(): void {
    if (this.#latEarlyStopCandidateStart >= 0)
      this.#latEarlyStopStart = this.#latEarlyStopCandidateStart;
    this.#latEarlyStopCandidateStart = -1;
  }

  #stableEvidenceStart(phase: Exclude<StagePhase, "bidirectional">): number {
    if (phase === "latency") return Math.max(0, this.#idleRtts.length - 1);
    if (phase === "download") return this.#latestEvidenceStart(this.#dl);
    return this.#latestEvidenceStart(this.#ul);
  }

  /** Stability is evaluated after ingest. Start the result window at the
   * observation that supplied that evidence, not at its end boundary. */
  #latestEvidenceStart(accum: PhaseAccum): number {
    return accum.evidenceMs > 0 ? accum.evidenceBeforeLatestMs : -1;
  }

  #latestBaseline(accum: PhaseAccum): StableBaseline {
    return {
      bytes: accum.bytesBeforeLatest,
      evidenceMs: accum.evidenceBeforeLatestMs,
    };
  }

  /* ================= RESULT REDUCTION ================= */

  /** Reduce a transfer phase to effective bytes over represented time. */
  throughputResult(
    phase: "download" | "upload",
    useStableWindow: boolean,
  ): ThroughputResult {
    const download = phase === "download";
    return this.#reduceTransfer(
      download ? this.#dl : this.#ul,
      download ? this.#dlStableStartMs : this.#ulStableStartMs,
      download ? this.#dlStableBaseline : this.#ulStableBaseline,
      useStableWindow,
      download ? this.#dlFinalScore : this.#ulFinalScore,
      this.#loadedLossPct(),
    );
  }

  /** A failed transfer retains its whole authoritative evidence only after the
   * named floor. It deliberately bypasses stable-window selection. */
  partialThroughputResult(
    phase: "download" | "upload",
  ): ThroughputResult | null {
    const accum = phase === "download" ? this.#dl : this.#ul;
    if (accum.evidenceMs < MIN_PARTIAL_TRANSFER_EVIDENCE_MS) return null;
    return this.#reduceTransfer(
      accum,
      -1,
      null,
      false,
      0,
      this.#loadedLossPct(),
    );
  }

  /** Under-load ping timeout percentage over the whole run. */
  #loadedLossPct(): number {
    return this.#loadedPings
      ? (this.#loadedPingsLost / this.#loadedPings) * 100
      : 0;
  }

  /** Reduce both bidirectional lanes with the common effective-rate reducer. */
  bidirectionalResult(useStableWindow: boolean): {
    down: ThroughputResult;
    up: ThroughputResult;
  } {
    const lossPct = this.#loadedLossPct();
    return {
      down: this.#reduceTransfer(
        this.#biDown,
        this.#biStableStartDownMs,
        this.#biStableBaselineDown,
        useStableWindow,
        this.#biFinalScore,
        lossPct,
      ),
      up: this.#reduceTransfer(
        this.#biUp,
        this.#biStableStartUpMs,
        this.#biStableBaselineUp,
        useStableWindow,
        this.#biFinalScore,
        lossPct,
      ),
    };
  }

  /** Reduce bidirectional lanes independently for a partial stage. A caller
   * must not turn one surviving lane into a combined headline. */
  partialBidirectionalResult(): {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
  } {
    const lossPct = this.#loadedLossPct();
    const reduce = (accum: PhaseAccum): ThroughputResult | null =>
      accum.evidenceMs >= MIN_PARTIAL_TRANSFER_EVIDENCE_MS
        ? this.#reduceTransfer(accum, -1, null, false, 0, lossPct)
        : null;
    return { down: reduce(this.#biDown), up: reduce(this.#biUp) };
  }

  /** Resolve the adaptive stable window used by the latency headline. */
  #windowStart(
    stableStart: number,
    earlyStopStart: number,
    adaptiveEnabled: boolean,
    arrLen: number,
  ): number {
    if (!adaptiveEnabled) return -1;
    if (earlyStopStart >= 0 && earlyStopStart < arrLen) {
      const stableThroughStop =
        stableStart >= 0 && stableStart <= earlyStopStart;
      return stableThroughStop ? earlyStopStart : -1;
    }
    return stableStart >= 0 && stableStart < arrLen ? stableStart : -1;
  }

  /** Shared effective-throughput reducer for every transfer direction. */
  #reduceTransfer(
    accum: PhaseAccum,
    stableStart: number,
    stableBaseline: StableBaseline | null,
    useStableWindow: boolean,
    finalScore: number,
    packetLossPct: number,
  ): ThroughputResult {
    const band = bandForState(stableStart >= 0, finalScore);
    if (accum.evidenceMs <= 0) {
      return {
        meanBytesPerSec: 0,
        peakBytesPerSec: 0,
        stabilityPct: 0,
        totalBytes: accum.bytes,
        reportedBytesPerSec: 0,
        fullAverageBytesPerSec: 0,
        method: "full-average",
        stabilityScore: finalScore,
        band,
        packetLossPct,
        serverAuthoritative: accum.serverAuthoritative || undefined,
      };
    }
    const full =
      accum.evidenceMs > 0 ? accum.bytes / (accum.evidenceMs / 1_000) : 0;
    const stableRatio = (): { rate: number; has: boolean } => {
      if (!useStableWindow || stableStart < 0 || !stableBaseline)
        return { rate: 0, has: false };
      const evidenceMs = accum.evidenceMs - stableBaseline.evidenceMs;
      const bytes = accum.bytes - stableBaseline.bytes;
      return {
        rate: evidenceMs > 0 ? bytes / (evidenceMs / 1_000) : 0,
        has: evidenceMs > 0,
      };
    };
    const stable = stableRatio();
    const hasStableEvidence = stable.has;
    const reported = hasStableEvidence ? stable.rate : full;
    const stability = transferConfidence([...accum.stabilityBuckets.rates]);
    const descriptiveStability =
      stability.sampleCount >= 2
        ? Math.max(0, Math.min(1, 1 - stability.varianceRatio))
        : 0;

    return {
      meanBytesPerSec: reported,
      peakBytesPerSec: accum.peakBytesPerSec,
      stabilityPct: descriptiveStability * 100,
      totalBytes: accum.bytes,
      reportedBytesPerSec: reported,
      fullAverageBytesPerSec: full,
      method: hasStableEvidence ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band,
      packetLossPct,
      serverAuthoritative: accum.serverAuthoritative || undefined,
    };
  }

  /** Reduce the latency phase to its result. `idleFallbackMs` is used as the
   *  headline only when there are no usable samples at all. */
  latencyResult(cfg: RunnerConfig, idleFallbackMs: number): LatencyResult {
    const stableStart = this.#latStableStartIndex;
    const earlyStopStart = this.#latEarlyStopStart;
    const finalScore = this.#latFinalScore;
    const all = this.#allRtts;
    const idle = this.#idleRtts;
    // Headline (median unloaded) follows the same window rule as throughput
    // (see #windowStart); min/p50/p95/jitter/loss stay whole-run descriptors.
    const windowStart = this.#windowStart(
      stableStart,
      earlyStopStart,
      cfg.adaptive.enabled,
      idle.length,
    );
    const useWindow = windowStart >= 0;
    const idleWindow = useWindow ? idle.slice(windowStart) : idle;
    const idleMs =
      median(idleWindow.length ? idleWindow : all) || idleFallbackMs;
    return {
      idleMs,
      minMs: all.length ? Math.min(...all) : 0,
      p50Ms: percentile(all, 50),
      p95Ms: percentile(all, 95),
      jitterMs: meanAbsDeviation(all),
      packetLossPct: this.#pingsTotal
        ? (this.#pingsLost / this.#pingsTotal) * 100
        : 0,
      reportedMs: idleMs,
      method: useWindow ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band: bandForState(stableStart >= 0, finalScore),
    };
  }

  /** Latency evidence is usable only when enough outcomes include a success. */
  partialLatencyResult(
    cfg: RunnerConfig,
    idleFallbackMs: number,
  ): LatencyResult | null {
    if (
      this.#pingsTotal < MIN_PARTIAL_LATENCY_OUTCOMES ||
      this.#allRtts.length < MIN_PARTIAL_LATENCY_SUCCESSES
    )
      return null;
    return this.latencyResult(
      { ...cfg, adaptive: { ...cfg.adaptive, enabled: false } },
      idleFallbackMs,
    );
  }

  /** Bufferbloat grade from authoritative idle-vs-loaded RTT evidence. */
  bufferbloatGrade(): BufferbloatGrade | null {
    if (!this.#idleRtts.length || !this.#loadedRtts.length) return null;
    const idleMs = median(this.#idleRtts);
    const loadedMs = median(this.#loadedRtts);
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
