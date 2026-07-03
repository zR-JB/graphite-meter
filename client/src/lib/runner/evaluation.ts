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

  // ---- early-stop arm points ----
  // The sample index at the moment `shouldExitPhase` first armed the glide for
  // that phase (-1 if early stop never armed this phase). Latched once per
  // phase — it never moves once set, unlike the stable-run trackers above,
  // which can re-open after a drop. Read at finish alongside the stable-run
  // index: still on the SAME (or an earlier-starting) stable run as when armed
  // (`stableStart >= 0 && stableStart <= earlyStopStart`) → the entire
  // early-stopping phase held stable, so average from the arm point to the
  // end. Otherwise stability broke at some point after arming (dropped and
  // never recovered, or dropped and re-latched on a LATER run) → average the
  // entire measurement phase instead, never just the smaller trailing window.
  #dlEarlyStopStart = -1;
  #ulEarlyStopStart = -1;
  #latEarlyStopStart = -1;
  #biEarlyStopStart = -1;

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
    this.#latEarlyStopStart = -1;
    this.#biEarlyStopStart = -1;
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
    if (nowStable && !wasStable) start = Math.max(0, arrLen - 1);
    else if (!nowStable && wasStable) start = -1;

    if (phase === "download") this.#dlStableStart = start;
    else if (phase === "upload") this.#ulStableStart = start;
    else if (phase === "bidirectional") this.#biStableStart = start;
    else this.#latStableStart = start;

    return nowStable;
  }

  /** Latch the sample index at which `shouldExitPhase` first armed the
   *  early-finish glide for this phase. Idempotent — only the FIRST call per
   *  phase sets it (the core only arms a glide once per phase anyway, but this
   *  guards against ever moving an already-latched arm point). */
  noteEarlyStop(phase: StagePhase): void {
    const arrLen = this.#sampleArrLen(phase);
    const start = Math.max(0, arrLen - 1);
    if (phase === "download") {
      if (this.#dlEarlyStopStart < 0) this.#dlEarlyStopStart = start;
    } else if (phase === "upload") {
      if (this.#ulEarlyStopStart < 0) this.#ulEarlyStopStart = start;
    } else if (phase === "bidirectional") {
      if (this.#biEarlyStopStart < 0) this.#biEarlyStopStart = start;
    } else {
      if (this.#latEarlyStopStart < 0) this.#latEarlyStopStart = start;
    }
  }

  /** The sample count so far for a phase's own confidence-window array — the
   *  same index space `trackStableRun`/`noteEarlyStop` latch their indices
   *  into. Bidi uses the COMBINED-rate array (the single stability signal),
   *  not either lane; the lanes are pushed in lock-step so the index still
   *  applies to each lane's own array at result time. */
  #sampleArrLen(phase: StagePhase): number {
    if (phase === "download") return this.#dl.bytesPerSecValues.length;
    if (phase === "upload") return this.#ul.bytesPerSecValues.length;
    if (phase === "bidirectional") return this.#phaseBytesPerSec.length;
    return this.#idleRtts.length;
  }

  /* ================= RESULT REDUCTION ================= */

  /**
   * Reduce a transfer phase's samples to its headline value. See
   * {@link #windowStart} for which window (early-stopping phase, full
   * measurement phase, or trailing stable run) backs the headline.
   */
  throughputResult(
    phase: "download" | "upload",
    cfg: RunnerConfig,
  ): ThroughputResult {
    const a = phase === "download" ? this.#dl : this.#ul;
    const stableStart =
      phase === "download" ? this.#dlStableStart : this.#ulStableStart;
    const earlyStopStart =
      phase === "download" ? this.#dlEarlyStopStart : this.#ulEarlyStopStart;
    const finalScore =
      phase === "download" ? this.#dlFinalScore : this.#ulFinalScore;
    return this.#reduceTransfer(
      a,
      stableStart,
      earlyStopStart,
      finalScore,
      cfg,
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

  /** Reduce the bidirectional phase's two lanes to a {down, up} result pair —
   *  each lane reuses the same transfer reducer as download/upload. Both lanes
   *  share the phase's single stable-run index (computed over the combined-rate
   *  window), so the trailing stable window is the same span of samples in each
   *  lane (the lanes are pushed in lock-step, so their array indices align). */
  bidirectionalResult(cfg: RunnerConfig): {
    down: ThroughputResult;
    up: ThroughputResult;
  } {
    const lossPct = this.#loadedLossPct();
    return {
      down: this.#reduceTransfer(
        this.#biDown,
        this.#biStableStart,
        this.#biEarlyStopStart,
        this.#biFinalScore,
        cfg,
        lossPct,
      ),
      up: this.#reduceTransfer(
        this.#biUp,
        this.#biStableStart,
        this.#biEarlyStopStart,
        this.#biFinalScore,
        cfg,
        lossPct,
      ),
    };
  }

  /** Resolve which window backs a phase's headline:
   *   - Early stop armed for this phase (`earlyStopStart ≥ 0`) AND the phase
   *     never dropped off its stable run since arming (`stableStart` is still
   *     the SAME or an earlier-starting run) → average the entire
   *     early-stopping phase, from the arm point to the end.
   *   - Early stop armed but stability broke at some point after arming
   *     (lost it entirely, or lost-then-regained on a LATER run) → average
   *     the entire measurement phase — never just the smaller trailing window
   *     a re-latched run would otherwise produce.
   *   - Early stop never armed (adaptive off, or the phase ran to its natural
   *     end without ever qualifying) → the pre-existing rule: the trailing
   *     stable-run window if still stable at finish, else the full average.
   *  Returns -1 for "use the full array", else the slice start index. */
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

  /** Shared transfer-phase reducer: turn a lane's sample buffer + its latched
   *  stable-run/early-stop indices into a {@link ThroughputResult}. */
  #reduceTransfer(
    a: PhaseAccum,
    stableStart: number,
    earlyStopStart: number,
    finalScore: number,
    cfg: RunnerConfig,
    packetLossPct: number,
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
        packetLossPct,
      };
    }
    const full = v.reduce((s, x) => s + x, 0) / v.length;
    const peak = Math.max(...v);
    const variance = v.reduce((s, x) => s + (x - full) ** 2, 0) / v.length;
    const cv = full > 0 ? Math.sqrt(variance) / full : 0;
    const stabilityPct = Math.max(0, Math.min(100, 100 - cv * 100));

    const windowStart = this.#windowStart(
      stableStart,
      earlyStopStart,
      cfg.adaptive.enabled,
      v.length,
    );
    const useWindow = windowStart >= 0;
    const window = useWindow ? v.slice(windowStart) : v;
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
      packetLossPct,
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
