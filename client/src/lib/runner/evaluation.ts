/* ============================================================
 * The Graphite Meter — Evaluation core (RunAccumulator)
 * Engine-agnostic accumulation + reduction of raw samples.
 *
 * Owns everything that turns a stream of raw throughput/latency
 * samples into headline results: per-phase accumulation, the
 * adaptive confidence windows, the hysteretic stable-run tracker,
 * and the final result reducers. It contains NO simulation and NO
 * network I/O — both the dummy and a real runner push their raw
 * samples in here so identical samples yield identical results.
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
  samples: { rate: number; bytes: number; seconds: number }[];
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
  // Latest per-lane rate, so each bidi push can record the COMBINED (down+up)
  // rate into the confidence window — the single stability signal for the phase.
  #biLastDown = 0;
  #biLastUp = 0;
  #idleRtts: number[] = [];
  #loadedRtts: number[] = [];
  #allRtts: number[] = [];
  #pingsTotal = 0;
  #pingsLost = 0;
  // Under-load pings only — feeds the transfer loss/retransmission factor.
  #loadedPings = 0;
  #loadedPingsLost = 0;

  // ---- per-phase confidence windows (reset each measured phase) ----
  #phaseBytesPerSec: number[] = [];
  #phaseRtts: number[] = [];
  #phasePings = 0;
  #phasePingsLost = 0;

  // ---- trailing contiguous stable-run trackers ----
  // Each holds the index into its phase's sample array where the *current*
  // stable run began (or -1 when not currently stable), plus the last stability
  // score seen.
  #dlStableStart = -1;
  #ulStableStart = -1;
  #latStableStart = -1;
  // Bidi tracks ONE stable run, over the combined-rate confidence window — the
  // phase has a single early-stop signal even though it reports two lanes.
  #biStableStart = -1;
  #dlFinalScore = 0;
  #ulFinalScore = 0;
  #latFinalScore = 0;
  #biFinalScore = 0;

  #dlEarlyStopStart = -1;
  #ulEarlyStopStart = -1;
  #biEarlyStopStart = -1;
  #biDownEarlyStopStart = -1;
  #biUpEarlyStopStart = -1;
  #dlEarlyStopBroken = false;
  #ulEarlyStopBroken = false;
  #biEarlyStopBroken = false;
  #latEarlyStopStart = -1;

  /** Reset all run state — call at the start of each run. */
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
    this.#dlEarlyStopStart = -1;
    this.#ulEarlyStopStart = -1;
    this.#biEarlyStopStart = -1;
    this.#biDownEarlyStopStart = -1;
    this.#biUpEarlyStopStart = -1;
    this.#dlEarlyStopBroken = false;
    this.#ulEarlyStopBroken = false;
    this.#biEarlyStopBroken = false;
    this.#latEarlyStopStart = -1;
    this.beginPhase();
  }

  /** Reset the per-phase confidence windows when a measured phase begins. */
  beginPhase(): void {
    this.#phaseBytesPerSec = [];
    this.#phaseRtts = [];
    this.#phasePings = 0;
    this.#phasePingsLost = 0;
    // Per-lane latest only matters within the bidi phase; clear on phase entry
    // so a fresh bidi phase doesn't combine against a stale lane value.
    this.#biLastDown = 0;
    this.#biLastUp = 0;
  }

  /* ================= SAMPLE INGEST ================= */

  /** Record a transfer sample: instantaneous bytes/sec plus exact bytes and
   *  duration for time-weighted final reduction, tagged with direction.
   *  In download/upload `dir` matches the phase; in bidirectional it routes the
   *  sample to the down or up lane and feeds the COMBINED rate (this lane +
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
    this.#phasePings++;
    if (lost) this.#phasePingsLost++;
    else if (!underLoad) this.#phaseRtts.push(rttMs);
  }

  /* ================= STABILITY ================= */

  /** Stability for the active measured phase over its current confidence
   *  window — the single signal the pip, the early-finish decision, and the
   *  result selection all read. */
  confidence(phase: StagePhase): ConfidenceScore | LatencyConfidenceScore {
    return phase === "latency"
      ? latencyConfidence(
          this.#phaseRtts,
          this.#phasePings,
          this.#phasePingsLost,
        )
      : transferConfidence(this.#phaseBytesPerSec);
  }

  /** Update the per-phase trailing-stable-run index from this tick's score,
   *  with hysteresis: the run opens (records the latest sample index) once the
   *  score crosses `stabilityThreshold` and closes (-1) only after it drops
   *  below `stabilityThreshold − STABILITY_HYSTERESIS` — so a score hovering at
   *  the boundary doesn't toggle the stable state. Returns the latched state;
   *  at finish a ≥0 index means "still on a stable plateau". */
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
    const arrLen = this.#sampleArrLen(phase);

    const wasStable = start >= 0;
    const nowStable = isStillStable(wasStable, score, cfg);
    if (!nowStable) {
      if (phase === "download" && this.#dlEarlyStopStart >= 0)
        this.#dlEarlyStopBroken = true;
      else if (phase === "upload" && this.#ulEarlyStopStart >= 0)
        this.#ulEarlyStopBroken = true;
      else if (phase === "bidirectional" && this.#biEarlyStopStart >= 0)
        this.#biEarlyStopBroken = true;
    }
    if (nowStable && !wasStable) start = Math.max(0, arrLen - 1);
    else if (!nowStable && wasStable) start = -1;

    if (phase === "download") this.#dlStableStart = start;
    else if (phase === "upload") this.#ulStableStart = start;
    else if (phase === "bidirectional") this.#biStableStart = start;
    else this.#latStableStart = start;

    return nowStable;
  }

  /** Latch where an uninterrupted early-finish glide armed. */
  noteEarlyStop(phase: StagePhase): void {
    const arrLen = this.#sampleArrLen(phase);
    const start = Math.max(0, arrLen - 1);
    if (phase === "download" && this.#dlEarlyStopStart < 0)
      this.#dlEarlyStopStart = start;
    else if (phase === "upload" && this.#ulEarlyStopStart < 0)
      this.#ulEarlyStopStart = start;
    else if (phase === "bidirectional" && this.#biEarlyStopStart < 0) {
      this.#biEarlyStopStart = start;
      this.#biDownEarlyStopStart = Math.max(0, this.#biDown.samples.length - 1);
      this.#biUpEarlyStopStart = Math.max(0, this.#biUp.samples.length - 1);
    } else if (phase === "latency" && this.#latEarlyStopStart < 0)
      this.#latEarlyStopStart = start;
  }

  /** The sample count so far for a phase's confidence-window array. */
  #sampleArrLen(phase: StagePhase): number {
    if (phase === "download") return this.#dl.samples.length;
    if (phase === "upload") return this.#ul.samples.length;
    if (phase === "bidirectional") return this.#phaseBytesPerSec.length;
    return this.#idleRtts.length;
  }

  /* ================= RESULT REDUCTION ================= */

  /** Reduce a transfer phase to effective bytes over represented time. */
  throughputResult(phase: "download" | "upload"): ThroughputResult {
    const a = phase === "download" ? this.#dl : this.#ul;
    const stableStart =
      phase === "download" ? this.#dlStableStart : this.#ulStableStart;
    const finalScore =
      phase === "download" ? this.#dlFinalScore : this.#ulFinalScore;
    return this.#reduceTransfer(
      a,
      stableStart,
      phase === "download" ? this.#dlEarlyStopStart : this.#ulEarlyStopStart,
      phase === "download" ? this.#dlEarlyStopStart : this.#ulEarlyStopStart,
      phase === "download" ? this.#dlEarlyStopBroken : this.#ulEarlyStopBroken,
      finalScore,
      this.#loadedLossPct(),
    );
  }

  /** Under-load packet-loss % over the whole run — the loss signal the transfer
   *  loss/retransmission compensation factor consumes. Loss under load is a link
   *  property, so the same figure is stamped on both transfer results. 0 when no
   *  loaded pings ran. */
  #loadedLossPct(): number {
    return this.#loadedPings
      ? (this.#loadedPingsLost / this.#loadedPings) * 100
      : 0;
  }

  /** Reduce both bidirectional lanes with the common effective-rate reducer. */
  bidirectionalResult(): {
    down: ThroughputResult;
    up: ThroughputResult;
  } {
    const lossPct = this.#loadedLossPct();
    return {
      down: this.#reduceTransfer(
        this.#biDown,
        this.#biStableStart,
        this.#biDownEarlyStopStart,
        this.#biEarlyStopStart,
        this.#biEarlyStopBroken,
        this.#biFinalScore,
        lossPct,
      ),
      up: this.#reduceTransfer(
        this.#biUp,
        this.#biStableStart,
        this.#biUpEarlyStopStart,
        this.#biEarlyStopStart,
        this.#biEarlyStopBroken,
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
    a: PhaseAccum,
    stableStart: number,
    earlyStopStart: number,
    earlyStabilityStart: number,
    earlyStopBroken: boolean,
    finalScore: number,
    packetLossPct: number,
  ): ThroughputResult {
    const v = a.samples.map((s) => s.rate);
    const band = bandForState(stableStart >= 0, finalScore);
    if (!v.length) {
      return {
        meanBytesPerSec: 0,
        peakBytesPerSec: 0,
        stabilityPct: 0,
        totalBytes: a.bytes,
        reportedBytesPerSec: 0,
        fullAverageBytesPerSec: 0,
        method: "full-average",
        stabilityScore: finalScore,
        band,
        packetLossPct,
        serverAuthoritative: a.serverAuthoritative || undefined,
      };
    }
    const ratio = (samples: PhaseAccum["samples"]): number => {
      const seconds = samples.reduce((sum, s) => sum + s.seconds, 0);
      return seconds > 0
        ? samples.reduce((sum, s) => sum + s.bytes, 0) / seconds
        : 0;
    };
    const full = ratio(a.samples);
    const earlyCompleted =
      earlyStopStart >= 0 &&
      earlyStopStart < a.samples.length &&
      !earlyStopBroken &&
      stableStart >= 0 &&
      stableStart <= earlyStabilityStart;
    const reported = earlyCompleted
      ? ratio(a.samples.slice(earlyStopStart))
      : full;
    const peak = Math.max(...v);
    const variance = v.reduce((s, x) => s + (x - full) ** 2, 0) / v.length;
    const cv = full > 0 ? Math.sqrt(variance) / full : 0;
    const stabilityPct = Math.max(0, Math.min(100, 100 - cv * 100));

    return {
      meanBytesPerSec: reported,
      peakBytesPerSec: peak,
      stabilityPct,
      totalBytes: a.bytes,
      reportedBytesPerSec: reported,
      fullAverageBytesPerSec: full,
      method: earlyCompleted ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band,
      packetLossPct,
      serverAuthoritative: a.serverAuthoritative || undefined,
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
