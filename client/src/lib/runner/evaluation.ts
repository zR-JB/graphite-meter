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
  bandForState,
  isStillStable,
  type ConfidenceScore,
  type LatencyConfidenceScore,
} from "./adaptive";
import { median, percentile, meanAbsDeviation } from "./stats";

/** Per-transfer-phase sample bookkeeping for the final result. */
interface PhaseAccum {
  samples: {
    rate: number;
    bytes: number;
    seconds: number;
    confidenceIndex: number;
  }[];
  bytes: number;
  serverAuthoritative: boolean;
}

export class RunAccumulator {
  // ---- whole-run result bookkeeping ----
  #dl: PhaseAccum = { samples: [], bytes: 0, serverAuthoritative: false };
  #ul: PhaseAccum = { samples: [], bytes: 0, serverAuthoritative: false };
  // Bidirectional carries TWO concurrent lanes (down + up), each reduced with
  // the same throughput reducer as a normal transfer phase.
  #biDown: PhaseAccum = { samples: [], bytes: 0, serverAuthoritative: false };
  #biUp: PhaseAccum = { samples: [], bytes: 0, serverAuthoritative: false };
  // Latest per-lane rate, so each bidi push records the COMBINED (down+up) rate
  // into the confidence window: the single stability signal for the phase.
  #biLastDown = 0;
  #biLastUp = 0;
  #idleRtts: number[] = [];
  #loadedRtts: number[] = [];
  #allRtts: number[] = [];
  #pingsTotal = 0;
  #pingsLost = 0;
  // Under-load ping timeouts are a quality signal, not inferred TCP loss.
  #loadedPings = 0;
  #loadedPingsLost = 0;

  // ---- per-phase confidence windows (reset each measured phase) ----
  #phaseBytesPerSec: number[] = [];
  #phaseLatency: (number | null)[] = [];

  // ---- trailing contiguous stable-run trackers ----
  // Each holds the index into its phase's sample array where the current stable
  // run starts, or -1 while it is not stable, plus the latest stability score.
  #dlStableStart = -1;
  #ulStableStart = -1;
  #latStableStart = -1;
  // Bidi tracks ONE stable run over the combined-rate confidence window: the
  // phase has a single early-stop signal even though it reports two lanes.
  #biStableStart = -1;
  #dlFinalScore = 0;
  #ulFinalScore = 0;
  #latFinalScore = 0;
  #biFinalScore = 0;

  // Latency retains its existing arm-to-end median rule. Throughput result
  // selection is independent of the glide arm and reads the final stable run.
  #latEarlyStopStart = -1;

  /** Reset all run state. Call at the start of each run. */
  reset(): void {
    this.#dl = { samples: [], bytes: 0, serverAuthoritative: false };
    this.#ul = { samples: [], bytes: 0, serverAuthoritative: false };
    this.#biDown = { samples: [], bytes: 0, serverAuthoritative: false };
    this.#biUp = { samples: [], bytes: 0, serverAuthoritative: false };
    this.#biLastDown = 0;
    this.#biLastUp = 0;
    this.#idleRtts = [];
    this.#loadedRtts = [];
    this.#allRtts = [];
    this.#pingsTotal = 0;
    this.#pingsLost = 0;
    this.#loadedPings = 0;
    this.#loadedPingsLost = 0;
    this.#dlStableStart = -1;
    this.#ulStableStart = -1;
    this.#latStableStart = -1;
    this.#biStableStart = -1;
    this.#dlFinalScore = 0;
    this.#ulFinalScore = 0;
    this.#latFinalScore = 0;
    this.#biFinalScore = 0;
    this.#latEarlyStopStart = -1;
    this.beginPhase();
  }

  /** Reset the per-phase confidence windows when a measured phase begins. */
  beginPhase(): void {
    this.#phaseBytesPerSec = [];
    this.#phaseLatency = [];
    // Per-lane latest only matters within the bidi phase; clear on phase entry
    // so a fresh bidi phase doesn't combine against a stale lane value.
    this.#biLastDown = 0;
    this.#biLastUp = 0;
  }

  /* ================= SAMPLE INGEST ================= */

  /** Record a transfer sample: instantaneous bytes/sec plus exact bytes and
   *  duration for time-weighted reduction, tagged with direction. Bidirectional
   *  routes the sample to its lane and feeds the COMBINED rate (this lane plus
   *  the other lane's latest) into the single confidence window. */
  pushThroughput(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    bytesPerSec: number,
    bytesDelta: number,
    durationSec: number,
    serverAuthoritative = false,
  ): void {
    if (durationSec <= 0) return;
    const sample = {
      rate: bytesPerSec,
      bytes: Math.max(0, bytesDelta),
      seconds: durationSec,
      // Bidi lanes have independent buffers but one interleaved confidence
      // stream. Carry its index on each sample so a final stable window can be
      // reduced without assuming the two lanes report in lockstep.
      confidenceIndex: this.#phaseBytesPerSec.length,
    };
    if (phase === "bidirectional") {
      const lane = dir === "down" ? this.#biDown : this.#biUp;
      lane.samples.push(sample);
      lane.bytes += sample.bytes;
      lane.serverAuthoritative ||= serverAuthoritative;
      if (dir === "down") this.#biLastDown = bytesPerSec;
      else this.#biLastUp = bytesPerSec;
      // One stability signal over the combined throughput (down + up).
      this.#phaseBytesPerSec.push(this.#biLastDown + this.#biLastUp);
      return;
    }
    const accum = phase === "download" ? this.#dl : this.#ul;
    accum.samples.push(sample);
    accum.bytes += sample.bytes;
    accum.serverAuthoritative ||= serverAuthoritative;
    this.#phaseBytesPerSec.push(bytesPerSec);
  }

  /** Record a ping sample. Latency confidence uses unloaded RTTs + the loss
   *  count over the same window. */
  pushLatency(rttMs: number, underLoad: boolean, lost: boolean): void {
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
    if (!underLoad) this.#phaseLatency.push(lost ? null : rttMs);
  }

  /* ================= STABILITY ================= */

  /** Stability for the active measured phase over its current confidence
   *  window: the single signal the pip, the early-finish decision, and the
   *  result selection all read. */
  confidence(phase: StagePhase): ConfidenceScore | LatencyConfidenceScore {
    return phase === "latency"
      ? latencyConfidence(this.#phaseLatency)
      : transferConfidence(this.#phaseBytesPerSec);
  }

  /** Update the per-phase trailing-stable-run index from this tick's score. The
   *  run opens at the latest sample index and closes to -1; `isStillStable`
   *  supplies the hysteresis. Returns the latched state, where at finish a ≥0
   *  index means the phase is still on a stable plateau. */
  trackStableRun(
    phase: StagePhase,
    score: number,
    cfg: RunnerConfig["adaptive"],
  ): boolean {
    let start: number;
    if (phase === "download") {
      this.#dlFinalScore = score;
      start = this.#dlStableStart;
    } else if (phase === "upload") {
      this.#ulFinalScore = score;
      start = this.#ulStableStart;
    } else if (phase === "bidirectional") {
      this.#biFinalScore = score;
      start = this.#biStableStart;
    } else {
      this.#latFinalScore = score;
      start = this.#latStableStart;
    }
    const sampleCount = this.#confidenceSampleCount(phase);

    const wasStable = start >= 0;
    const nowStable = isStillStable(wasStable, score, cfg);
    if (nowStable && !wasStable) start = Math.max(0, sampleCount - 1);
    else if (!nowStable && wasStable) start = -1;

    if (phase === "download") this.#dlStableStart = start;
    else if (phase === "upload") this.#ulStableStart = start;
    else if (phase === "bidirectional") this.#biStableStart = start;
    else this.#latStableStart = start;

    return nowStable;
  }

  /** Preserve latency's existing arm-to-end result window. Throughput result
   *  selection deliberately does not depend on when its glide armed. */
  noteLatencyEarlyStop(): void {
    if (this.#latEarlyStopStart < 0) {
      this.#latEarlyStopStart = Math.max(0, this.#idleRtts.length - 1);
    }
  }

  /** The sample count so far for a phase's confidence-window array. */
  #confidenceSampleCount(phase: StagePhase): number {
    if (phase === "download") return this.#dl.samples.length;
    if (phase === "upload") return this.#ul.samples.length;
    if (phase === "bidirectional") return this.#phaseBytesPerSec.length;
    return this.#idleRtts.length;
  }

  /* ================= RESULT REDUCTION ================= */

  /** Reduce a transfer phase to effective bytes over represented time. */
  throughputResult(
    phase: "download" | "upload",
    adaptiveEnabled: boolean,
  ): ThroughputResult {
    const download = phase === "download";
    return this.#reduceTransfer(
      download ? this.#dl : this.#ul,
      download ? this.#dlStableStart : this.#ulStableStart,
      adaptiveEnabled,
      download ? this.#dlFinalScore : this.#ulFinalScore,
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
  bidirectionalResult(adaptiveEnabled: boolean): {
    down: ThroughputResult;
    up: ThroughputResult;
  } {
    const lossPct = this.#loadedLossPct();
    return {
      down: this.#reduceTransfer(
        this.#biDown,
        this.#biStableStart,
        adaptiveEnabled,
        this.#biFinalScore,
        lossPct,
      ),
      up: this.#reduceTransfer(
        this.#biUp,
        this.#biStableStart,
        adaptiveEnabled,
        this.#biFinalScore,
        lossPct,
      ),
    };
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
    adaptiveEnabled: boolean,
    finalScore: number,
    packetLossPct: number,
  ): ThroughputResult {
    const rates = accum.samples.map((s) => s.rate);
    const band = bandForState(stableStart >= 0, finalScore);
    if (!rates.length) {
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
    const ratio = (samples: PhaseAccum["samples"]): number => {
      const seconds = samples.reduce((sum, s) => sum + s.seconds, 0);
      return seconds > 0
        ? samples.reduce((sum, s) => sum + s.bytes, 0) / seconds
        : 0;
    };
    const full = ratio(accum.samples);
    const stableSamples =
      adaptiveEnabled && stableStart >= 0
        ? accum.samples.filter(
            (sample) => sample.confidenceIndex >= stableStart,
          )
        : [];
    const useStableWindow = stableSamples.length > 0;
    const reported = useStableWindow ? ratio(stableSamples) : full;
    const variance =
      rates.reduce((sum, rate) => sum + (rate - full) ** 2, 0) / rates.length;
    const cv = full > 0 ? Math.sqrt(variance) / full : 0;

    return {
      meanBytesPerSec: reported,
      peakBytesPerSec: Math.max(...rates),
      stabilityPct: Math.max(0, Math.min(100, 100 - cv * 100)),
      totalBytes: accum.bytes,
      reportedBytesPerSec: reported,
      fullAverageBytesPerSec: full,
      method: useStableWindow ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band,
      packetLossPct,
      serverAuthoritative: accum.serverAuthoritative || undefined,
    };
  }

  /** Reduce the latency phase to its result. `idleFallbackMs` is used as the
   *  headline only when there are no usable samples at all. */
  latencyResult(cfg: RunnerConfig, idleFallbackMs: number): LatencyResult {
    const stableStart = this.#latStableStart;
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

  /** Bufferbloat grade from the idle-vs-loaded RTT delta. `idleFallbackMs` is
   *  used only when no idle samples exist. */
  bufferbloatGrade(idleFallbackMs: number): BufferbloatGrade {
    const idleMs =
      median(this.#idleRtts.length ? this.#idleRtts : this.#allRtts) ||
      idleFallbackMs;
    const loadedMs = this.#loadedRtts.length
      ? median(this.#loadedRtts)
      : idleMs;
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
