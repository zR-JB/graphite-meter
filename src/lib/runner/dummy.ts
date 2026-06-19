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
} from "./contract";
import {
  shouldExitPhase,
  bandForState,
  type ConfidenceScore,
  type LatencyConfidenceScore,
} from "./adaptive";
import {
  buildSegments,
  rebuildTail,
  segmentAt,
  type Segment,
} from "./schedule";
import { RunAccumulator } from "./evaluation";

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
  downBytesPerSec: number;
  upBytesPerSec: number;
  idleRttMs: number;
  /** RTT increase under load — drives the bufferbloat grade. */
  loadedDeltaMs: number;
  /** Baseline loss probability per ping. */
  lossBase: number;
  /** Relative std of the plateau (throughput and idle RTT) — how *steady* the
   *  link is. This is what the adaptive stability score reads: steady links
   *  (fiber/cable) settle to a high band and finish early on the stable window;
   *  jittery ones (lte/satellite) never settle and report the full average. */
  jitter: number;
}

// Throughput is bytes/sec (browser-native). Link rates are conventionally
// quoted in bits/sec, so the trailing comment notes the familiar bit-rate.
const PROFILES: Record<NonNullable<DummyOptions["profile"]>, ProfileSpec> = {
  fiber: { downBytesPerSec: 117.5e6, upBytesPerSec: 110e6, idleRttMs: 6, loadedDeltaMs: 4, lossBase: 0.0, jitter: 0.04 }, // ~940/880 Mbit/s
  cable: { downBytesPerSec: 40e6, upBytesPerSec: 2.75e6, idleRttMs: 16, loadedDeltaMs: 34, lossBase: 0.002, jitter: 0.05 }, // ~320/22 Mbit/s
  lte: { downBytesPerSec: 8e6, upBytesPerSec: 3e6, idleRttMs: 38, loadedDeltaMs: 62, lossBase: 0.01, jitter: 0.09 }, // ~64/24 Mbit/s
  satellite: { downBytesPerSec: 13.75e6, upBytesPerSec: 1.75e6, idleRttMs: 600, loadedDeltaMs: 180, lossBase: 0.015, jitter: 0.11 }, // ~110/14 Mbit/s
  throttled: { downBytesPerSec: 1.1875e6, upBytesPerSec: 0.5625e6, idleRttMs: 28, loadedDeltaMs: 48, lossBase: 0.005, jitter: 0.05 }, // ~9.5/4.5 Mbit/s
};

const PING_INTERVAL: Record<RunnerConfig["pingConcurrency"], number> = {
  instant: 80,
  medium: 250,
  slow: 600,
};

const THROUGHPUT_CADENCE_MS = 60; // ≈16Hz
const TICK_MS = 20; // master loop resolution
const STABILITY_CADENCE_MS = 100; // ≈10Hz — pip emit rate (predicate runs every tick)

/* ---------- Live anomaly defaults (§13.6) ----------
 * Construction-time anomalies (DummyOptions.anomalies) fire at phase fractions.
 * These are the defaults for RUNTIME anomalies injected mid-run via
 * `injectAnomaly` — each occupies an absolute [start,end) window measured in
 * ms since run start, computed from "now" when the Developer button is hit. */
const LIVE_ANOMALY_DEFAULTS = {
  latencySpike: { magnitude: 3, durationMs: 600 }, // rtt ×3 for 600ms
  packetLoss: { magnitude: 0.6, durationMs: 900 }, // 60% loss probability
  throughputDrop: { magnitude: 0.4, durationMs: 600 }, // bytesPerSec −40%
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
  #lastStabilityAt = -Infinity;
  #bytesCumulative = 0;
  #cfg: RunnerConfig | null = null;

  // Live, dev-injected anomalies (§13.6). Each is an absolute [start,end)
  // window on the effective timeline; the synthesis hooks read this list.
  #liveAnomalies: LiveAnomaly[] = [];

  // ---- Virtual-time clock (§13.4) ----
  // The tick loop advances a virtual timeline each tick. At rest it tracks
  // wall-clock 1:1. When a measured phase becomes confidently stable an
  // early-finish "glide" is armed for that segment: virtual time is driven
  // along an eased curve to the phase's end over a short real-time window
  // (adaptive.glideMs), so it crosses seg.end — and eventually #totalMs —
  // sooner in wall-clock. The progress marker visibly accelerates and the run
  // genuinely finishes early, without ever faking a bar to 1.0 (coverage stays
  // truthful, §13.4). Replaces the old global #shiftMs offset, whose sign
  // pulled elapsed backward and trapped a stable phase in an infinite reset.
  #virtualElapsed = 0; // ms of virtual timeline consumed
  #lastRealNow = 0; // performance.now() at the previous tick
  // Glide state, bound to one segment by its index.
  #glideArmedForSeg = -1; // segment index a glide is armed for, or -1
  #glideStartReal = 0; // performance.now() when the glide armed
  #glideFromVirtual = 0; // #virtualElapsed at arm time
  #glideTargetVirtual = 0; // virtual position to glide to (seg.end)

  // Engine-agnostic accumulation + reduction (sample windows, stable-run
  // tracking, result reducers). The dummy feeds it raw synthesized samples;
  // a real runner feeds it real ones, so both reduce identically.
  #accum = new RunAccumulator();

  // Per-stage results, computed + emitted once the moment each measured phase
  // ends (independent of the others) and reused to build the final RunResult.
  #dlResult: ThroughputResult | null = null;
  #ulResult: ThroughputResult | null = null;
  #latResult: LatencyResult | null = null;

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

    // Build the phase timeline, skipping disabled stages (shared scheduler).
    const { segments, totalMs } = buildSegments(config);
    this.#segments = segments;
    this.#totalMs = totalMs;
    this.#t0 = performance.now();
    this.#lastRealNow = this.#t0;

    this.#tickTimer = setInterval(() => this.#tick(), TICK_MS);
    this.#tick(); // emit the first transition immediately
  }

  #resetRunState() {
    this.#lastEmittedPhase = "idle";
    this.#lastThroughputAt = -Infinity;
    this.#lastPingAt = -Infinity;
    this.#lastStabilityAt = -Infinity;
    this.#bytesCumulative = 0;
    this.#accum.reset();
    this.#dlResult = null;
    this.#ulResult = null;
    this.#latResult = null;
    this.#virtualElapsed = 0;
    this.#lastRealNow = 0;
    this.#glideArmedForSeg = -1;
    this.#glideStartReal = 0;
    this.#glideFromVirtual = 0;
    this.#glideTargetVirtual = 0;
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
    const elapsed = this.#virtualElapsed;
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

    // Rebuild only the future tail of the timeline (shared scheduler).
    const { segments, totalMs } = rebuildTail(this.#segments, this.#virtualElapsed, this.#cfg);
    this.#segments = segments;
    this.#totalMs = totalMs;
    // Segment indices shifted under us — drop any armed glide so it re-arms
    // against the rebuilt array on a later tick.
    this.#glideArmedForSeg = -1;
  }

  /* ================= MASTER TICK ================= */
  #tick() {
    // Advance the virtual clock. Normally 1:1 with wall-clock; while an
    // early-finish glide is armed the position is driven directly along an
    // eased curve toward the current phase's end (§13.4).
    const now = performance.now();
    const dtReal = now - this.#lastRealNow;
    this.#lastRealNow = now;
    // Always advance at least 1:1; an armed glide then pulls the position
    // further forward along its eased curve (never backward, never slower).
    this.#virtualElapsed += dtReal;
    if (this.#glideArmedForSeg >= 0) this.#advanceGlide(now);
    const elapsed = this.#virtualElapsed;

    if (elapsed >= this.#totalMs) {
      this.#finish();
      return;
    }

    const seg = segmentAt(this.#segments, elapsed);
    if (!seg) return;

    // Phase transition?
    if (seg.phase !== this.#lastEmittedPhase) {
      // The phase we're leaving has just finished — emit its final per-stage
      // result before announcing the transition, so its card resolves now.
      this.#finalizeStage(this.#lastEmittedPhase);
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

    // Throughput (download / upload only). Cadence is gated on REAL time, not
    // virtual: during an early-finish glide virtual time races ahead, and
    // gating on it would dump a whole tail's worth of samples into the canvas
    // at once (the "clunky fast-forward" feel). Real-time gating keeps the
    // emit rate steady; the accelerated tail just carries fewer, sparser
    // samples (its `t` still advances on the virtual clock). (§13.4)
    if (
      (seg.phase === "download" || seg.phase === "upload") &&
      now - this.#lastThroughputAt >= THROUGHPUT_CADENCE_MS
    ) {
      this.#lastThroughputAt = now;
      this.#emitThroughput(seg, elapsed);
    }

    // Latency pings (latency + under-load during dl/ul). Loaded pings are
    // suppressed when latency is fully off (stage off + skip-with-stage on),
    // so disabling the latency stage removes bufferbloat sampling too.
    const cfg = this.#cfg!;
    const loadedLatency = cfg.stages.latency || !cfg.skipLoadedLatencyWhenStageOff;
    const pingInterval = PING_INTERVAL[cfg.pingConcurrency];
    const pingActive =
      seg.phase === "latency" ||
      ((seg.phase === "download" || seg.phase === "upload") && loadedLatency);
    if (pingActive && now - this.#lastPingAt >= pingInterval) {
      this.#lastPingAt = now;
      this.#emitLatency(seg, elapsed);
    }

    // Stability is computed ONCE per tick for the active measured phase and
    // drives BOTH the live pip (emitted, throttled) and the early-finish glide
    // — one signal, no second meaning to reconcile (§13.4).
    if (seg.phase === "latency" || seg.phase === "download" || seg.phase === "upload") {
      const conf: ConfidenceScore | LatencyConfidenceScore = this.#accum.confidence(seg.phase);
      // Track the trailing stable run FIRST so the emitted band reflects the
      // hysteretic latched state (entering stable takes a higher bar than
      // leaving — the pip and the stable window don't flicker). (§13.4)
      const stable = this.#accum.trackStableRun(seg.phase, conf.score, this.#cfg!.adaptive);
      if (now - this.#lastStabilityAt >= STABILITY_CADENCE_MS) {
        this.#lastStabilityAt = now;
        this.#emit({
          type: "stability",
          snapshot: {
            phase: seg.phase,
            score: conf.score,
            band: bandForState(stable, conf.score),
            sampleCount: conf.sampleCount,
          },
        });
      }
      // Adaptive early finish: once confidently stable, arm a glide that
      // accelerates virtual time to the segment boundary (§13.4).
      this.#maybeArmGlide(seg, elapsed, conf);
    }
  }

  /** Reset the per-phase confidence windows when a measured phase begins. */
  #beginAdaptivePhase() {
    this.#glideArmedForSeg = -1; // a new phase never inherits the prior glide
    this.#accum.beginPhase();
  }

  /** Evaluate the adaptive early-finish predicate; arm a glide if stable.
   *  `conf` is the stability already computed for this tick (shared with the
   *  pip emit) so the gate and the displayed band can never disagree. */
  #maybeArmGlide(
    seg: Segment,
    elapsed: number,
    conf: ConfidenceScore | LatencyConfidenceScore,
  ) {
    const cfg = this.#cfg!;
    if (!cfg.adaptive.enabled) return;
    if (this.#glideArmedForSeg >= 0) return; // already gliding this phase

    const kind: "latency" | "transfer" = seg.phase === "latency" ? "latency" : "transfer";
    const exit = shouldExitPhase({
      kind,
      elapsedMs: elapsed - seg.start,
      durationMs: seg.end - seg.start,
      confidence: conf,
      cfg: cfg.adaptive,
    });
    if (!exit) return;

    // Arm the glide for this segment: drive virtual time from here to the phase
    // end over #glideMs of real time (eased). We never fake progress to 1.0 —
    // the marker accelerates and the next phase takes over on arrival.
    this.#glideArmedForSeg = this.#segments.indexOf(seg);
    this.#glideStartReal = performance.now();
    this.#glideFromVirtual = elapsed;
    this.#glideTargetVirtual = seg.end;
  }

  /** Drive the virtual clock along the armed glide's eased curve. */
  #advanceGlide(now: number) {
    const glideMs = Math.max(1, this.#cfg!.adaptive.glideMs);
    const p = Math.min(1, Math.max(0, (now - this.#glideStartReal) / glideMs));
    const eased = easeInOutCubic(p);
    const target =
      this.#glideFromVirtual + (this.#glideTargetVirtual - this.#glideFromVirtual) * eased;
    // Monotonic: a jittery tick must never rewind the marker.
    this.#virtualElapsed = Math.max(this.#virtualElapsed, target);
  }

  #setPhase(p: Phase) {
    this.#phase = p;
  }

  /** Compute, cache, and emit a measured phase's final result exactly once —
   *  the moment it ends. Each stage is independent: it reads only its own
   *  samples + stable-run index + final score. No-op for warmup/non-run stages
   *  and for an already-finalized stage. */
  #finalizeStage(phase: Phase) {
    const cfg = this.#cfg!;
    if (phase === "download" && cfg.stages.download && !this.#dlResult) {
      this.#dlResult = this.#accum.throughputResult("download", cfg);
      this.#emit({ type: "stageResult", stage: "download", result: this.#dlResult });
    } else if (phase === "upload" && cfg.stages.upload && !this.#ulResult) {
      this.#ulResult = this.#accum.throughputResult("upload", cfg);
      this.#emit({ type: "stageResult", stage: "upload", result: this.#ulResult });
    } else if (phase === "latency" && cfg.stages.latency && !this.#latResult) {
      this.#latResult = this.#accum.latencyResult(cfg, this.#spec.idleRttMs);
      this.#emit({ type: "stageResult", stage: "latency", result: this.#latResult });
    }
  }

  /* ---------- Throughput sample synthesis ---------- */
  #emitThroughput(seg: Segment, elapsed: number) {
    const tp = elapsed - seg.start; // ms into this phase
    const phaseLen = seg.end - seg.start;
    const mean = seg.phase === "download" ? this.#spec.downBytesPerSec : this.#spec.upBytesPerSec;

    // Logistic ramp-up over the first ~1.2s.
    const ramp = 1 / (1 + Math.exp(-(tp - 600) / 150));

    // Noisy plateau, Gaussian noise scaled by the profile's steadiness.
    let bytesPerSec = mean * ramp * (1 + this.#gauss() * this.#spec.jitter);

    // Throughput dip anomaly: 400ms 40% drop centred on each fraction.
    for (const f of this.#opts.anomalies?.throughputDipAt ?? []) {
      const center = f * phaseLen;
      if (tp >= center && tp < center + 400) bytesPerSec *= 0.6;
    }

    // Live throughput-drop anomaly (§13.6): reduce bytesPerSec by `magnitude` over its
    // window. Fired relative to the current moment, not a phase fraction.
    const drop = this.#activeAnomaly("throughput-drop", elapsed);
    if (drop) bytesPerSec *= Math.max(0, 1 - drop.magnitude);

    bytesPerSec = Math.max(0, bytesPerSec);

    // Accumulate cumulative bytes over the cadence window (rate is bytes/sec).
    const dtSec = THROUGHPUT_CADENCE_MS / 1000;
    const bytes = bytesPerSec * dtSec;
    this.#bytesCumulative += bytes;

    // Accumulate into the shared evaluation core (whole-phase + confidence
    // window). seg.phase is download/upload here (gated by the tick).
    this.#accum.pushThroughput(seg.phase as "download" | "upload", bytesPerSec, bytes);

    const sample: ThroughputSample = {
      t: elapsed,
      bytesPerSec,
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
    rtt *= 1 + this.#gauss() * this.#spec.jitter; // jitter scaled by profile steadiness

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

    // Accumulate into the shared evaluation core (whole-run distribution +
    // per-phase confidence window).
    this.#accum.pushLatency(rtt, underLoad, lost);

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
    // The final measured phase ends here — finalize it like any other (the
    // earlier phases finalized at their transitions), then assemble RunResult
    // from the cached per-stage results so the aggregate and the per-stage
    // events never disagree.
    this.#finalizeStage(this.#lastEmittedPhase);
    // Actual wall-clock length — shorter than the nominal #totalMs whenever an
    // adaptive glide accelerated one or more phases to an early finish (§13.4).
    const actualMs = Math.max(0, performance.now() - this.#t0);
    const result: RunResult = {
      download: this.#dlResult,
      upload: this.#ulResult,
      // Latency is always present in the aggregate: when the latency STAGE ran
      // it was finalized as a stage result; otherwise compute it here from any
      // under-load pings (bufferbloat still needs it).
      latency: this.#latResult ?? this.#accum.latencyResult(cfg, this.#spec.idleRttMs),
      bufferbloat: this.#accum.bufferbloatGrade(this.#spec.idleRttMs),
      startedAt: Date.now() - actualMs,
      durationMs: actualMs,
    };

    this.#setPhase("complete");
    this.#lastEmittedPhase = "complete";
    this.#emit({ type: "complete", result });
  }

}

/* ---------- small simulation helpers ---------- */
/** Smooth ease-in-out cubic on [0,1]; drives the early-finish glide curve. */
function easeInOutCubic(p: number): number {
  return p < 0.5 ? 4 * p * p * p : 1 - (-2 * p + 2) ** 3 / 2;
}
