/* ============================================================
 * The Graphite Meter — Dummy Runner (§2.2)
 * Deterministic-seedable development engine. Emits the same
 * RunnerEvent stream a real engine would, so the entire UI can
 * be built and tested without a backend. Swapping to a real
 * engine touches only wire.ts.
 * ============================================================ */

import type {
  NetworkRunner,
  RunnerConfig,
  RunnerEvent,
  RunnerAnomaly,
  Phase,
  PhaseTransition,
  InfraInfo,
  ThroughputSample,
  LatencySample,
  RunResult,
  ThroughputResult,
  LatencyResult,
  BufferbloatGrade,
} from "./contract";
import {
  transferConfidence,
  latencyConfidence,
  shouldExitPhase,
} from "./adaptive";

export interface DummyOptions {
  seed?: number;
  /** Inject anomalies at fractions of each phase: */
  anomalies?: {
    packetDropAt?: number[]; // e.g. [0.4, 0.7] → drop bursts
    latencySpikeAt?: number[]; // e.g. [0.5] → 3× rtt spike
    throughputDipAt?: number[]; // e.g. [0.6] → 400ms 40% drop
  };
  /** Realistic target profiles: */
  profile?: "fiber" | "cable" | "lte" | "satellite" | "throttled";
}

/* ---------- Profile table: mean throughput + latency character ---------- */
interface ProfileSpec {
  downBps: number;
  upBps: number;
  idleRttMs: number;
  /** RTT increase under load — drives the bufferbloat grade. */
  loadedDeltaMs: number;
  /** Baseline loss probability per ping. */
  lossBase: number;
}

const PROFILES: Record<NonNullable<DummyOptions["profile"]>, ProfileSpec> = {
  fiber: { downBps: 940e6, upBps: 880e6, idleRttMs: 6, loadedDeltaMs: 4, lossBase: 0.0 },
  cable: { downBps: 320e6, upBps: 22e6, idleRttMs: 16, loadedDeltaMs: 34, lossBase: 0.002 },
  lte: { downBps: 64e6, upBps: 24e6, idleRttMs: 38, loadedDeltaMs: 62, lossBase: 0.01 },
  satellite: { downBps: 110e6, upBps: 14e6, idleRttMs: 600, loadedDeltaMs: 180, lossBase: 0.015 },
  throttled: { downBps: 9.5e6, upBps: 4.5e6, idleRttMs: 28, loadedDeltaMs: 48, lossBase: 0.005 },
};

const PING_INTERVAL: Record<RunnerConfig["pingConcurrency"], number> = {
  instant: 80,
  medium: 250,
  slow: 600,
};

const THROUGHPUT_CADENCE_MS = 60; // ≈16Hz
const TICK_MS = 20; // master loop resolution

/* ---------- Live anomaly defaults (§13.6) ----------
 * Construction-time anomalies (DummyOptions.anomalies) fire at phase fractions.
 * These are the defaults for RUNTIME anomalies injected mid-run via
 * `injectAnomaly` — each occupies an absolute [start,end) window measured in
 * ms since run start, computed from "now" when the Developer button is hit. */
const LIVE_ANOMALY_DEFAULTS = {
  latencySpike: { magnitude: 3, durationMs: 600 }, // rtt ×3 for 600ms
  packetLoss: { magnitude: 0.6, durationMs: 900 }, // 60% loss probability
  throughputDrop: { magnitude: 0.4, durationMs: 600 }, // bps −40%
} as const;

/** A scheduled live anomaly with an absolute window on the run timeline. */
interface LiveAnomaly {
  kind: RunnerAnomaly["kind"];
  start: number; // ms since run start
  end: number;
  magnitude: number;
}

/* ---------- Deterministic RNG (mulberry32 + Box–Muller) ---------- */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/* ---------- Internal phase segment on the timeline ---------- */
interface Segment {
  phase: Extract<Phase, "warmup" | "latency" | "download" | "upload">;
  start: number; // ms offset from run start
  end: number;
}

/* ---------- Per-phase sample bookkeeping for the final result ---------- */
interface PhaseAccum {
  bpsValues: number[];
  bytes: number;
}

export class DummyRunner implements NetworkRunner {
  #handlers = new Set<(e: RunnerEvent) => void>();
  #phase: Phase = "idle";

  #opts: Required<Pick<DummyOptions, "profile">> & DummyOptions;
  #spec: ProfileSpec;
  #rand: () => number;

  // run-time state
  #tickTimer: ReturnType<typeof setInterval> | null = null;
  #t0 = 0; // performance.now() at run start
  #segments: Segment[] = [];
  #totalMs = 0;
  #lastEmittedPhase: Phase = "idle";
  #lastThroughputAt = -Infinity;
  #lastPingAt = -Infinity;
  #bytesCumulative = 0;
  #cfg: RunnerConfig | null = null;

  // Live, dev-injected anomalies (§13.6). Each is an absolute [start,end)
  // window on the effective timeline; the synthesis hooks read this list.
  #liveAnomalies: LiveAnomaly[] = [];

  // Adaptive early-exit: time removed from the timeline by phases that
  // exited early. Effective elapsed = realElapsed − #shiftMs, which slides
  // every later segment earlier so the run finishes sooner WITHOUT ever
  // faking a phase's progress to 1.0 — coverage stays truthful (§13.4).
  #shiftMs = 0;
  // Per-phase sample windows feeding the confidence math (reset per phase).
  #phaseBps: number[] = [];
  #phaseRtts: number[] = [];
  #phasePings = 0;
  #phasePingsLost = 0;

  // result bookkeeping
  #dl: PhaseAccum = { bpsValues: [], bytes: 0 };
  #ul: PhaseAccum = { bpsValues: [], bytes: 0 };
  #idleRtts: number[] = [];
  #loadedRtts: number[] = [];
  #allRtts: number[] = [];
  #pingsTotal = 0;
  #pingsLost = 0;

  constructor(opts: DummyOptions = {}) {
    this.#opts = { profile: opts.profile ?? "fiber", ...opts };
    this.#spec = PROFILES[this.#opts.profile];
    this.#rand = mulberry32(opts.seed ?? 0x9e3779b9);
  }

  get phase(): Phase {
    return this.#phase;
  }

  on(handler: (e: RunnerEvent) => void): () => void {
    this.#handlers.add(handler);
    return () => this.#handlers.delete(handler);
  }

  #emit(e: RunnerEvent) {
    for (const h of this.#handlers) h(e);
  }

  /* ---------- Gaussian noise via Box–Muller ---------- */
  #gauss(): number {
    const u = Math.max(1e-9, this.#rand());
    const v = this.#rand();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  }

  /* ================= PROBE ================= */
  async probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo> {
    const interval = 90;
    const pings = 4;
    // Emit a few pre-test pings so the sparkline has something to show.
    for (let i = 0; i < pings; i++) {
      await new Promise((r) => setTimeout(r, interval));
      const rtt = this.#spec.idleRttMs * (1 + this.#gauss() * 0.08);
      this.#emit({
        type: "latency",
        sample: { t: -interval * (pings - i), rttMs: rtt, underLoad: false, lost: false },
      });
    }

    const host = endpoint.host === "auto" ? "edge-fra-03.graphite.net" : endpoint.host;
    const octet = () => Math.floor(this.#rand() * 254) + 1;
    return {
      clientIp: `${octet()}.${octet()}.${octet()}.${octet()}`,
      server: {
        name: "Graphite Edge — Frankfurt",
        host,
        port: endpoint.port,
        location: "Frankfurt, DE",
      },
      preTestPingMs: this.#spec.idleRttMs,
      engineVersion: "dummy-1.0.0",
      protocolNegotiated:
        this.#opts.profile === "satellite" ? "h3 (QUIC)" : "webtransport/h3",
    };
  }

  /* ================= START ================= */
  start(config: RunnerConfig): void {
    if (this.#tickTimer) this.abort();
    this.#cfg = config;
    this.#resetRunState();

    // Build the phase timeline, skipping disabled stages.
    const segs: Segment[] = [];
    let cursor = 0;
    const push = (phase: Segment["phase"], ms: number) => {
      if (ms <= 0) return;
      segs.push({ phase, start: cursor, end: cursor + ms });
      cursor += ms;
    };
    push("warmup", config.duration.warmupMs);
    if (config.stages.latency) push("latency", config.duration.latencyMs);
    if (config.stages.download) push("download", config.duration.downloadMs);
    if (config.stages.upload) push("upload", config.duration.uploadMs);

    this.#segments = segs;
    this.#totalMs = cursor;
    this.#t0 = performance.now();

    this.#tickTimer = setInterval(() => this.#tick(), TICK_MS);
    this.#tick(); // emit the first transition immediately
  }

  #resetRunState() {
    this.#lastEmittedPhase = "idle";
    this.#lastThroughputAt = -Infinity;
    this.#lastPingAt = -Infinity;
    this.#bytesCumulative = 0;
    this.#dl = { bpsValues: [], bytes: 0 };
    this.#ul = { bpsValues: [], bytes: 0 };
    this.#idleRtts = [];
    this.#loadedRtts = [];
    this.#allRtts = [];
    this.#pingsTotal = 0;
    this.#pingsLost = 0;
    this.#shiftMs = 0;
    this.#phaseBps = [];
    this.#phaseRtts = [];
    this.#phasePings = 0;
    this.#phasePingsLost = 0;
    this.#liveAnomalies = [];
  }

  /* ================= LIVE ANOMALY INJECTION (§13.6) ================= */
  /**
   * Fire a dev-only anomaly into the in-flight run. It opens an absolute
   * window starting at the current effective elapsed and is consumed by the
   * existing synthesis hooks (#emitThroughput / #emitLatency). No-op when not
   * running, so the Developer panel's disabled state mirrors `isRunning`.
   */
  injectAnomaly(a: RunnerAnomaly): void {
    if (!this.#tickTimer) return;
    const elapsed = performance.now() - this.#t0 - this.#shiftMs;
    const d =
      a.kind === "latency-spike"
        ? LIVE_ANOMALY_DEFAULTS.latencySpike
        : a.kind === "packet-loss"
          ? LIVE_ANOMALY_DEFAULTS.packetLoss
          : LIVE_ANOMALY_DEFAULTS.throughputDrop;
    const durationMs = a.durationMs ?? d.durationMs;
    this.#liveAnomalies.push({
      kind: a.kind,
      start: elapsed,
      end: elapsed + durationMs,
      magnitude: a.magnitude ?? d.magnitude,
    });
  }

  /** The currently-active live anomaly of a given kind, if any. Also prunes
   *  windows that have fully elapsed so the list stays bounded. */
  #activeAnomaly(kind: RunnerAnomaly["kind"], elapsed: number): LiveAnomaly | null {
    if (this.#liveAnomalies.length) {
      this.#liveAnomalies = this.#liveAnomalies.filter((x) => elapsed < x.end);
    }
    for (const x of this.#liveAnomalies) {
      if (x.kind === kind && elapsed >= x.start && elapsed < x.end) return x;
    }
    return null;
  }

  /* ================= ABORT ================= */
  abort(): void {
    if (!this.#tickTimer) return;
    clearInterval(this.#tickTimer);
    this.#tickTimer = null;
    const from = this.#phase;
    this.#setPhase("aborted");
    this.#emit({
      type: "phase",
      transition: { from, to: "aborted", t: performance.now() - this.#t0 },
    });
  }

  /* ================= LIVE STAGE RECONFIGURE (§13.4) ================= */
  /**
   * Apply a live change to the enabled stage set mid-run. Only FUTURE
   * segments (those that start after the current effective elapsed) are
   * rebuilt; the current and past phases are untouched, so toggling a
   * not-yet-started stage off simply shortens the remaining timeline.
   * No-op when idle (the next start() snapshot already reflects the change).
   */
  reconfigureStages(stages: RunnerConfig["stages"]): void {
    if (!this.#tickTimer || !this.#cfg) return;
    this.#cfg = { ...this.#cfg, stages };

    const elapsed = performance.now() - this.#t0 - this.#shiftMs;
    // Keep every segment that has already started; rebuild the tail.
    const kept = this.#segments.filter((s) => s.start <= elapsed);
    let cursor = kept.length ? kept[kept.length - 1].end : 0;

    const dur = this.#cfg.duration;
    const tail: Segment[] = [];
    const pushIf = (
      on: boolean,
      phase: Segment["phase"],
      ms: number,
    ) => {
      // Skip phases already kept (started) and disabled phases.
      if (!on || ms <= 0) return;
      if (kept.some((k) => k.phase === phase)) return;
      tail.push({ phase, start: cursor, end: cursor + ms });
      cursor += ms;
    };
    pushIf(stages.latency, "latency", dur.latencyMs);
    pushIf(stages.download, "download", dur.downloadMs);
    pushIf(stages.upload, "upload", dur.uploadMs);

    this.#segments = [...kept, ...tail];
    this.#totalMs = cursor;
  }

  /* ================= MASTER TICK ================= */
  #tick() {
    // Effective elapsed = real elapsed minus the time reclaimed by phases that
    // exited early. The whole timeline (and the configured total) shrinks by
    // #shiftMs, so progress fractions and the finish point stay honest.
    const elapsed = performance.now() - this.#t0 - this.#shiftMs;

    if (elapsed >= this.#totalMs) {
      this.#finish();
      return;
    }

    const seg = this.#segments.find((s) => elapsed >= s.start && elapsed < s.end);
    if (!seg) return;

    // Phase transition?
    if (seg.phase !== this.#lastEmittedPhase) {
      const transition: PhaseTransition = {
        from: this.#lastEmittedPhase,
        to: seg.phase,
        t: elapsed,
      };
      this.#lastEmittedPhase = seg.phase;
      this.#setPhase(seg.phase);
      this.#emit({ type: "phase", transition });
      this.#beginAdaptivePhase();
    }

    // Progress within the current phase (real coverage — never faked).
    const frac = (elapsed - seg.start) / (seg.end - seg.start);
    this.#emit({ type: "progress", phase: seg.phase, fraction: Math.min(1, Math.max(0, frac)) });

    // Throughput (download / upload only, 60ms cadence).
    if (
      (seg.phase === "download" || seg.phase === "upload") &&
      elapsed - this.#lastThroughputAt >= THROUGHPUT_CADENCE_MS
    ) {
      this.#lastThroughputAt = elapsed;
      this.#emitThroughput(seg, elapsed);
    }

    // Latency pings (latency + under-load during dl/ul).
    const pingInterval = PING_INTERVAL[this.#cfg!.pingConcurrency];
    const pingActive =
      seg.phase === "latency" || seg.phase === "download" || seg.phase === "upload";
    if (pingActive && elapsed - this.#lastPingAt >= pingInterval) {
      this.#lastPingAt = elapsed;
      this.#emitLatency(seg, elapsed);
    }

    // Adaptive early exit: once this phase is confidently stable, reclaim the
    // rest of its nominal window by growing #shiftMs to the segment boundary.
    // Next tick lands in the following segment (or finishes) cleanly.
    this.#maybeExitEarly(seg, elapsed);
  }

  /** Reset the per-phase confidence windows when a measured phase begins. */
  #beginAdaptivePhase() {
    this.#phaseBps = [];
    this.#phaseRtts = [];
    this.#phasePings = 0;
    this.#phasePingsLost = 0;
  }

  /** Evaluate the adaptive early-exit predicate for the current segment. */
  #maybeExitEarly(seg: Segment, elapsed: number) {
    const cfg = this.#cfg!;
    if (!cfg.adaptive.enabled) return;

    const durationMs = seg.end - seg.start;
    const elapsedInPhase = elapsed - seg.start;

    let exit = false;
    if (seg.phase === "latency") {
      const conf = latencyConfidence(this.#phaseRtts, this.#phasePings, this.#phasePingsLost);
      exit = shouldExitPhase({
        kind: "latency",
        elapsedMs: elapsedInPhase,
        durationMs,
        confidence: conf,
        cfg: cfg.adaptive,
      });
    } else if (seg.phase === "download" || seg.phase === "upload") {
      const conf = transferConfidence(this.#phaseBps);
      exit = shouldExitPhase({
        kind: "transfer",
        elapsedMs: elapsedInPhase,
        durationMs,
        confidence: conf,
        cfg: cfg.adaptive,
      });
    }
    if (!exit) return;

    // Reclaim the unused tail of this segment. We do NOT emit a progress=1;
    // the phase ends at its real coverage and the next phase takes over.
    this.#shiftMs += seg.end - elapsed;
  }

  #setPhase(p: Phase) {
    this.#phase = p;
  }

  /* ---------- Throughput sample synthesis ---------- */
  #emitThroughput(seg: Segment, elapsed: number) {
    const tp = elapsed - seg.start; // ms into this phase
    const phaseLen = seg.end - seg.start;
    const mean = seg.phase === "download" ? this.#spec.downBps : this.#spec.upBps;

    // Logistic ramp-up over the first ~1.2s.
    const ramp = 1 / (1 + Math.exp(-(tp - 600) / 150));

    // Noisy plateau, Gaussian ±8% of mean.
    let bps = mean * ramp * (1 + this.#gauss() * 0.08);

    // Throughput dip anomaly: 400ms 40% drop centred on each fraction.
    for (const f of this.#opts.anomalies?.throughputDipAt ?? []) {
      const center = f * phaseLen;
      if (tp >= center && tp < center + 400) bps *= 0.6;
    }

    // Live throughput-drop anomaly (§13.6): reduce bps by `magnitude` over its
    // window. Fired relative to the current moment, not a phase fraction.
    const drop = this.#activeAnomaly("throughput-drop", elapsed);
    if (drop) bps *= Math.max(0, 1 - drop.magnitude);

    bps = Math.max(0, bps);

    // Accumulate cumulative bytes (bits → bytes over the cadence window).
    const dtSec = THROUGHPUT_CADENCE_MS / 1000;
    const bytes = (bps / 8) * dtSec;
    this.#bytesCumulative += bytes;

    const accum = seg.phase === "download" ? this.#dl : this.#ul;
    accum.bpsValues.push(bps);
    accum.bytes += bytes;

    // Feed the adaptive confidence window for the current transfer phase.
    this.#phaseBps.push(bps);

    const sample: ThroughputSample = {
      t: elapsed,
      bps,
      bytesCumulative: this.#bytesCumulative,
      streamCount: this.#cfg!.parallelStreams,
    };
    this.#emit({ type: "throughput", sample });
  }

  /* ---------- Latency sample synthesis ---------- */
  #emitLatency(seg: Segment, elapsed: number) {
    const tp = elapsed - seg.start;
    const phaseLen = seg.end - seg.start;
    const frac = tp / phaseLen;
    const underLoad = seg.phase === "download" || seg.phase === "upload";

    let rtt = this.#spec.idleRttMs;
    if (underLoad) {
      // Under-load latency rises toward the profile's loaded delta.
      const loadRamp = 1 / (1 + Math.exp(-(tp - 500) / 200));
      rtt += this.#spec.loadedDeltaMs * loadRamp;
    }
    rtt *= 1 + this.#gauss() * 0.06; // jitter

    // Latency spike anomaly: 3× RTT near each fraction.
    for (const f of this.#opts.anomalies?.latencySpikeAt ?? []) {
      if (Math.abs(frac - f) < 0.02) rtt *= 3;
    }

    // Live latency-spike anomaly (§13.6): scale rtt by `magnitude` in-window.
    const spike = this.#activeAnomaly("latency-spike", elapsed);
    if (spike) rtt *= spike.magnitude;

    // Packet loss: baseline + burst windows.
    let lossProb = this.#spec.lossBase;
    for (const f of this.#opts.anomalies?.packetDropAt ?? []) {
      if (Math.abs(frac - f) < 0.03) lossProb = 0.6;
    }
    // Live packet-loss anomaly (§13.6): raise loss probability in-window.
    const drop = this.#activeAnomaly("packet-loss", elapsed);
    if (drop) lossProb = Math.max(lossProb, drop.magnitude);
    const lost = this.#rand() < lossProb;

    this.#pingsTotal++;
    if (lost) {
      this.#pingsLost++;
    } else {
      this.#allRtts.push(rtt);
      if (underLoad) this.#loadedRtts.push(rtt);
      else this.#idleRtts.push(rtt);
    }

    // Feed the adaptive confidence window for the current phase. Latency
    // confidence uses unloaded RTTs + the loss count over the same window.
    this.#phasePings++;
    if (lost) this.#phasePingsLost++;
    else if (!underLoad) this.#phaseRtts.push(rtt);

    const sample: LatencySample = { t: elapsed, rttMs: rtt, underLoad, lost };
    this.#emit({ type: "latency", sample });
  }

  /* ================= FINISH → RunResult ================= */
  #finish() {
    if (this.#tickTimer) {
      clearInterval(this.#tickTimer);
      this.#tickTimer = null;
    }

    const cfg = this.#cfg!;
    // Actual wall-clock length — shorter than the nominal #totalMs whenever
    // adaptive early-exit reclaimed time (#shiftMs).
    const actualMs = Math.max(0, this.#totalMs - this.#shiftMs);
    const result: RunResult = {
      download: cfg.stages.download ? this.#throughputResult(this.#dl) : null,
      upload: cfg.stages.upload ? this.#throughputResult(this.#ul) : null,
      latency: this.#latencyResult(),
      bufferbloat: this.#bufferbloatGrade(),
      startedAt: Date.now() - actualMs,
      durationMs: actualMs,
    };

    this.#setPhase("complete");
    this.#lastEmittedPhase = "complete";
    this.#emit({ type: "complete", result });
  }

  #throughputResult(a: PhaseAccum): ThroughputResult {
    const v = a.bpsValues;
    if (!v.length) {
      return { meanBps: 0, peakBps: 0, stabilityPct: 0, totalBytes: a.bytes };
    }
    const mean = v.reduce((s, x) => s + x, 0) / v.length;
    const peak = Math.max(...v);
    const variance = v.reduce((s, x) => s + (x - mean) ** 2, 0) / v.length;
    const cv = mean > 0 ? Math.sqrt(variance) / mean : 0;
    const stabilityPct = Math.max(0, Math.min(100, 100 - cv * 100));
    return { meanBps: mean, peakBps: peak, stabilityPct, totalBytes: a.bytes };
  }

  #latencyResult(): LatencyResult {
    const all = this.#allRtts;
    const idleMs = median(this.#idleRtts.length ? this.#idleRtts : all) || this.#spec.idleRttMs;
    return {
      idleMs,
      minMs: all.length ? Math.min(...all) : 0,
      p50Ms: percentile(all, 50),
      p95Ms: percentile(all, 95),
      jitterMs: meanAbsDeviation(all),
      packetLossPct: this.#pingsTotal ? (this.#pingsLost / this.#pingsTotal) * 100 : 0,
    };
  }

  #bufferbloatGrade(): BufferbloatGrade {
    const idleMs = median(this.#idleRtts.length ? this.#idleRtts : this.#allRtts) || this.#spec.idleRttMs;
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

/* ---------- small statistics helpers ---------- */
function median(xs: number[]): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

function percentile(xs: number[], p: number): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const idx = Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1));
  return s[idx];
}

function meanAbsDeviation(xs: number[]): number {
  if (xs.length < 2) return 0;
  let acc = 0;
  for (let i = 1; i < xs.length; i++) acc += Math.abs(xs[i] - xs[i - 1]);
  return acc / (xs.length - 1);
}
