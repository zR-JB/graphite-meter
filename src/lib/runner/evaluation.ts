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
  bytesPerSecValues: number[];
  bytes: number;
}

export class RunAccumulator {
  // ---- whole-run result bookkeeping ----
  #dl: PhaseAccum = { bytesPerSecValues: [], bytes: 0 };
  #ul: PhaseAccum = { bytesPerSecValues: [], bytes: 0 };
  // Bidirectional carries TWO concurrent lanes (down + up), each reduced with
  // the same throughput reducer as a normal transfer phase.
  #biDown: PhaseAccum = { bytesPerSecValues: [], bytes: 0 };
  #biUp: PhaseAccum = { bytesPerSecValues: [], bytes: 0 };
  // Latest per-lane rate, so each bidi push can record the COMBINED (down+up)
  // rate into the confidence window — the single stability signal for the phase.
  #biLastDown = 0;
  #biLastUp = 0;
  #idleRtts: number[] = [];
  #loadedRtts: number[] = [];
  #allRtts: number[] = [];
  #pingsTotal = 0;
  #pingsLost = 0;

  // ---- per-phase confidence windows (reset each measured phase) ----
  #phaseBytesPerSec: number[] = [];
  #phaseRtts: number[] = [];
  #phasePings = 0;
  #phasePingsLost = 0;

  // ---- trailing contiguous stable-run trackers (§13.4) ----
  // Each holds the index into its phase's sample array where the *current*
  // stable run began (or -1 when not currently stable), plus the last stability
  // score seen. Read at finish: still stable (≥0) → average the trailing window;
  // lost it (-1) → full avg.
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

  /** Reset all run state — call at the start of each run. */
  reset(): void {
    this.#dl = { bytesPerSecValues: [], bytes: 0 };
    this.#ul = { bytesPerSecValues: [], bytes: 0 };
    this.#biDown = { bytesPerSecValues: [], bytes: 0 };
    this.#biUp = { bytesPerSecValues: [], bytes: 0 };
    this.#biLastDown = 0;
    this.#biLastUp = 0;
    this.#idleRtts = [];
    this.#loadedRtts = [];
    this.#allRtts = [];
    this.#pingsTotal = 0;
    this.#pingsLost = 0;
    this.#dlStableStart = -1;
    this.#ulStableStart = -1;
    this.#latStableStart = -1;
    this.#biStableStart = -1;
    this.#dlFinalScore = 0;
    this.#ulFinalScore = 0;
    this.#latFinalScore = 0;
    this.#biFinalScore = 0;
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

  /** Record a transfer sample: instantaneous bytes/sec plus the bytes
   *  transferred over the cadence window it represents, tagged with direction.
   *  In download/upload `dir` matches the phase; in bidirectional it routes the
   *  sample to the down or up lane and feeds the COMBINED rate (this lane +
   *  the other lane's latest) into the single confidence window. */
  pushThroughput(
    phase: "download" | "upload" | "bidirectional",
    dir: FlowDirection,
    bytesPerSec: number,
    bytesDelta: number,
  ): void {
    if (phase === "bidirectional") {
      const lane = dir === "down" ? this.#biDown : this.#biUp;
      lane.bytesPerSecValues.push(bytesPerSec);
      lane.bytes += bytesDelta;
      if (dir === "down") this.#biLastDown = bytesPerSec;
      else this.#biLastUp = bytesPerSec;
      // One stability signal over the combined throughput (down + up).
      this.#phaseBytesPerSec.push(this.#biLastDown + this.#biLastUp);
      return;
    }
    const accum = phase === "download" ? this.#dl : this.#ul;
    accum.bytesPerSecValues.push(bytesPerSec);
    accum.bytes += bytesDelta;
    this.#phaseBytesPerSec.push(bytesPerSec);
  }

  /** Record a ping sample. Latency confidence uses unloaded RTTs + the loss
   *  count over the same window. */
  pushLatency(rttMs: number, underLoad: boolean, lost: boolean): void {
    this.#pingsTotal++;
    if (lost) {
      this.#pingsLost++;
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
      ? latencyConfidence(this.#phaseRtts, this.#phasePings, this.#phasePingsLost)
      : transferConfidence(this.#phaseBytesPerSec);
  }

  /** Update the per-phase trailing-stable-run index from this tick's score,
   *  with hysteresis: the run opens (records the latest sample index) once the
   *  score crosses `stabilityThreshold` and closes (-1) only after it drops
   *  below `stabilityThreshold − STABILITY_HYSTERESIS` — so a score hovering at
   *  the boundary doesn't toggle the stable state. Returns the latched state;
   *  at finish a ≥0 index means "still on a stable plateau". */
  trackStableRun(phase: StagePhase, score: number, cfg: RunnerConfig["adaptive"]): boolean {
    let start: number;
    let arrLen: number;
    if (phase === "download") {
      this.#dlFinalScore = score;
      start = this.#dlStableStart;
      arrLen = this.#dl.bytesPerSecValues.length;
    } else if (phase === "upload") {
      this.#ulFinalScore = score;
      start = this.#ulStableStart;
      arrLen = this.#ul.bytesPerSecValues.length;
    } else if (phase === "bidirectional") {
      // The stable-run index is into the COMBINED-rate confidence window (the
      // single signal), not either lane — so the trailing window slices both
      // lanes consistently at result time.
      this.#biFinalScore = score;
      start = this.#biStableStart;
      arrLen = this.#phaseBytesPerSec.length;
    } else {
      this.#latFinalScore = score;
      start = this.#latStableStart;
      arrLen = this.#idleRtts.length;
    }

    const wasStable = start >= 0;
    const nowStable = isStillStable(wasStable, score, cfg);
    if (nowStable && !wasStable) start = Math.max(0, arrLen - 1);
    else if (!nowStable && wasStable) start = -1;

    if (phase === "download") this.#dlStableStart = start;
    else if (phase === "upload") this.#ulStableStart = start;
    else if (phase === "bidirectional") this.#biStableStart = start;
    else this.#latStableStart = start;

    return nowStable;
  }

  /* ================= RESULT REDUCTION ================= */

  /**
   * Reduce a transfer phase's samples to its headline value. When adaptive is
   * on and the phase was still on a stable plateau at finish (`stableStart ≥ 0`)
   * the headline is the mean over that trailing window — the steady plateau,
   * not the ramp-up-diluted whole. Otherwise (adaptive off, or stability lost
   * before the end) it falls back to the full-phase average (§13.4).
   */
  throughputResult(phase: "download" | "upload", cfg: RunnerConfig): ThroughputResult {
    const a = phase === "download" ? this.#dl : this.#ul;
    const stableStart = phase === "download" ? this.#dlStableStart : this.#ulStableStart;
    const finalScore = phase === "download" ? this.#dlFinalScore : this.#ulFinalScore;
    return this.#reduceTransfer(a, stableStart, finalScore, cfg);
  }

  /** Reduce the bidirectional phase's two lanes to a {down, up} result pair —
   *  each lane reuses the same transfer reducer as download/upload. Both lanes
   *  share the phase's single stable-run index (computed over the combined-rate
   *  window), so the trailing stable window is the same span of samples in each
   *  lane (the lanes are pushed in lock-step, so their array indices align). */
  bidirectionalResult(cfg: RunnerConfig): { down: ThroughputResult; up: ThroughputResult } {
    return {
      down: this.#reduceTransfer(this.#biDown, this.#biStableStart, this.#biFinalScore, cfg),
      up: this.#reduceTransfer(this.#biUp, this.#biStableStart, this.#biFinalScore, cfg),
    };
  }

  /** Shared transfer-phase reducer: turn a lane's sample buffer + its latched
   *  stable-run index into a {@link ThroughputResult}. The headline is the mean
   *  over the trailing stable window when adaptive is on and the lane was still
   *  stable at finish, else the full-phase average (§13.4). */
  #reduceTransfer(
    a: PhaseAccum,
    stableStart: number,
    finalScore: number,
    cfg: RunnerConfig,
  ): ThroughputResult {
    const v = a.bytesPerSecValues;
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
      };
    }
    const full = v.reduce((s, x) => s + x, 0) / v.length;
    const peak = Math.max(...v);
    const variance = v.reduce((s, x) => s + (x - full) ** 2, 0) / v.length;
    const cv = full > 0 ? Math.sqrt(variance) / full : 0;
    const stabilityPct = Math.max(0, Math.min(100, 100 - cv * 100));

    const useWindow = cfg.adaptive.enabled && stableStart >= 0 && stableStart < v.length;
    const window = useWindow ? v.slice(stableStart) : v;
    const reported = window.reduce((s, x) => s + x, 0) / window.length;

    return {
      meanBytesPerSec: reported,
      peakBytesPerSec: peak,
      stabilityPct,
      totalBytes: a.bytes,
      reportedBytesPerSec: reported,
      fullAverageBytesPerSec: full,
      method: useWindow ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band,
    };
  }

  /** Reduce the latency phase to its result. `idleFallbackMs` is used as the
   *  headline only when there are no usable samples at all. */
  latencyResult(cfg: RunnerConfig, idleFallbackMs: number): LatencyResult {
    const stableStart = this.#latStableStart;
    const finalScore = this.#latFinalScore;
    const all = this.#allRtts;
    const idle = this.#idleRtts;
    const useWindow = cfg.adaptive.enabled && stableStart >= 0 && stableStart < idle.length;
    // Headline (median unloaded) follows the same stable-window vs full rule;
    // min/p50/p95/jitter/loss stay whole-run distribution descriptors.
    const idleWindow = useWindow ? idle.slice(stableStart) : idle;
    const idleMs = median(idleWindow.length ? idleWindow : all) || idleFallbackMs;
    return {
      idleMs,
      minMs: all.length ? Math.min(...all) : 0,
      p50Ms: percentile(all, 50),
      p95Ms: percentile(all, 95),
      jitterMs: meanAbsDeviation(all),
      packetLossPct: this.#pingsTotal ? (this.#pingsLost / this.#pingsTotal) * 100 : 0,
      reportedMs: idleMs,
      method: useWindow ? "stable-window" : "full-average",
      stabilityScore: finalScore,
      band: bandForState(stableStart >= 0, finalScore),
    };
  }

  /** Bufferbloat grade from the idle-vs-loaded RTT delta. `idleFallbackMs` is
   *  used only when no idle samples exist. */
  bufferbloatGrade(idleFallbackMs: number): BufferbloatGrade {
    const idleMs = median(this.#idleRtts.length ? this.#idleRtts : this.#allRtts) || idleFallbackMs;
    const loadedMs = this.#loadedRtts.length ? median(this.#loadedRtts) : idleMs;
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
