/* ============================================================
 * The Graphite Meter — Runner Core (§2.1 / §13.4)
 * The engine-agnostic orchestrator. Owns EVERYTHING that is not
 * network I/O: the phase timeline, phase sequencing + live
 * reconfiguration, the run clock (incl. the early-finish glide),
 * sample accumulation, stability detection, the early-stop
 * decision, result reduction, and the entire RunnerEvent stream.
 *
 * A `RunnerBackend` plugs in the ONLY engine-specific part — the
 * samples. The dummy synthesizes them; a real engine measures them
 * off the wire. Both push raw samples into the core, so identical
 * samples always yield identical results.
 *
 * Lifecycle (core → backend), so the backend only reacts:
 *   onRunStart → onPhaseEnter/onPhaseExit per phase → onComplete
 *   (or onAbort). A pull-style backend may implement onTick to
 *   synthesize per tick; a push-style (network) backend leaves it
 *   off and calls host.ingest* from its own I/O callbacks.
 * ============================================================ */

import type {
  NetworkRunner,
  RunnerConfig,
  RunnerEvent,
  RunnerAnomaly,
  Phase,
  PhaseTransition,
  InfraInfo,
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
  type StagePhase,
} from "./schedule";
import { RunAccumulator } from "./evaluation";

const TICK_MS = 20; // master loop resolution
const STABILITY_CADENCE_MS = 100; // ≈10Hz — pip emit rate (predicate runs every tick)

/** Per-tick context handed to a pull-style backend so it can synthesize the
 *  samples due this tick. `realNow` lets a backend gate sample cadence on real
 *  time (e.g. during the glide, virtual time races but real time does not). */
export interface TickContext {
  phase: Segment["phase"];
  /** absolute virtual ms since run start */
  elapsed: number;
  segStart: number;
  segEnd: number;
  /** performance.now() captured this tick */
  realNow: number;
}

/** The handle a backend uses to push raw samples / emit events into the core. */
export interface CoreHost {
  /** Push a measured throughput sample: instantaneous bytes/sec plus the bytes
   *  transferred over the interval it represents. The core tags it with the
   *  current phase + elapsed, accumulates it, and emits the throughput event. */
  ingestThroughput(bytesPerSec: number, bytesDelta: number): void;
  /** Push a measured ping: RTT, whether captured under load, and whether lost. */
  ingestLatency(rttMs: number, underLoad: boolean, lost: boolean): void;
  /** Emit a raw event directly (pre-test pings during probe, connectivity, …).
   *  Bypasses accumulation — use ingest* for measured run samples. */
  emit(e: RunnerEvent): void;
  /** Report an unrecoverable failure; the core ends the run into "error".
   *  (Batch 3 enriches the payload into a structured RunnerError.) */
  fail(message: string): void;
  /** The active config, or null when idle. */
  readonly config: RunnerConfig | null;
  /** The current lifecycle phase. */
  readonly phase: Phase;
  /** Absolute virtual ms since run start (the run clock) — for time-anchored
   *  backend logic such as live anomaly windows. */
  readonly elapsed: number;
}

/** A pluggable sample source + connection manager. The core drives it; it only
 *  reacts and pushes samples back through the {@link CoreHost}. */
export interface RunnerBackend {
  /** Receive the host handle (push samples / emit through it). Called once. */
  attach(host: CoreHost): void;
  /** Pre-test handshake; resolves InfraInfo. MAY emit a few pre-test `latency`
   *  samples (underLoad:false, negative `t`) via the host for the sparkline. */
  probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo>;
  /** A run is starting with this config. Per-phase priming happens in
   *  onPhaseEnter; reset any per-run state here. */
  onRunStart(config: RunnerConfig): void;
  /** A phase has begun. For a warmup, `warmupFor` names the stage being primed.
   *  Open/prime the connection(s) the phase needs. */
  onPhaseEnter(phase: Segment["phase"], warmupFor?: StagePhase): void;
  /** A phase has ended (boundary, early finish, or run end). Close its I/O. */
  onPhaseExit(phase: Segment["phase"]): void;
  /** The run finished normally. Clean up anything still open. */
  onComplete(): void;
  /** The run was aborted by the user. Cancel in-flight I/O and clean up. */
  onAbort(): void;
  /** OPTIONAL pull hook: called every tick for the active phase. A synthesizing
   *  backend produces samples here; a network backend leaves it undefined and
   *  pushes from its own callbacks instead. */
  onTick?(ctx: TickContext): void;
  /** OPTIONAL — react to a live stage-set change (open/close future-stage I/O).
   *  The core has already rebuilt the timeline before calling this. */
  onReconfigure?(stages: RunnerConfig["stages"]): void;
  /** OPTIONAL — fallback idle RTT, used only when a run yields no usable latency
   *  samples (e.g. the profile/preflight ping). */
  idleHintMs?(): number;
  /** OPTIONAL — fire a live dev anomaly (§13.6); real engines may omit it. */
  injectAnomaly?(a: RunnerAnomaly): void;
}

export class RunnerCore implements NetworkRunner, CoreHost {
  #handlers = new Set<(e: RunnerEvent) => void>();
  #phase: Phase = "idle";
  #backend: RunnerBackend;
  #cfg: RunnerConfig | null = null;

  // ---- run-time state ----
  #tickTimer: ReturnType<typeof setInterval> | null = null;
  #t0 = 0; // performance.now() at run start
  #segments: Segment[] = [];
  #totalMs = 0;
  #lastEmittedPhase: Phase = "idle";
  #lastStabilityAt = -Infinity;
  #bytesCumulative = 0;

  // ---- virtual-time clock + early-finish glide (§13.4) ----
  // The tick loop advances a virtual timeline each tick, at rest 1:1 with
  // wall-clock. When a measured phase becomes confidently stable an early-finish
  // "glide" is armed for that segment: virtual time is driven along an eased
  // curve to the phase's end over a short real-time window (adaptive.glideMs),
  // so it crosses seg.end — and eventually #totalMs — sooner in wall-clock. The
  // progress marker visibly accelerates and the run genuinely finishes early,
  // without ever faking a bar to 1.0 (coverage stays truthful).
  #virtualElapsed = 0;
  #lastRealNow = 0;
  #glideArmedForSeg = -1; // segment index a glide is armed for, or -1
  #glideStartReal = 0;
  #glideFromVirtual = 0;
  #glideTargetVirtual = 0;

  // ---- evaluation + result bookkeeping ----
  #accum = new RunAccumulator();
  #dlResult: ThroughputResult | null = null;
  #ulResult: ThroughputResult | null = null;
  #latResult: LatencyResult | null = null;

  constructor(backend: RunnerBackend) {
    this.#backend = backend;
    backend.attach(this);
  }

  /* ================= NetworkRunner surface ================= */
  get phase(): Phase {
    return this.#phase;
  }

  get config(): RunnerConfig | null {
    return this.#cfg;
  }

  get elapsed(): number {
    return this.#virtualElapsed;
  }

  on(handler: (e: RunnerEvent) => void): () => void {
    this.#handlers.add(handler);
    return () => this.#handlers.delete(handler);
  }

  emit(e: RunnerEvent): void {
    for (const h of this.#handlers) h(e);
  }

  probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo> {
    return this.#backend.probe(endpoint);
  }

  injectAnomaly(a: RunnerAnomaly): void {
    this.#backend.injectAnomaly?.(a);
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

    this.#backend.onRunStart(config);
    this.#tickTimer = setInterval(() => this.#tick(), TICK_MS);
    this.#tick(); // emit the first transition immediately
  }

  #resetRunState() {
    this.#lastEmittedPhase = "idle";
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
  }

  /* ================= ABORT ================= */
  abort(): void {
    if (!this.#tickTimer) return;
    clearInterval(this.#tickTimer);
    this.#tickTimer = null;
    const from = this.#phase;
    this.#backend.onAbort();
    this.#setPhase("aborted");
    this.emit({
      type: "phase",
      transition: { from, to: "aborted", t: performance.now() - this.#t0 },
    });
  }

  /* ================= LIVE STAGE RECONFIGURE (§13.4) ================= */
  /**
   * Apply a live change to the enabled stage set mid-run. Only FUTURE segments
   * (those starting after the current elapsed) are rebuilt; the current and past
   * phases are untouched, so toggling a not-yet-started stage off simply shortens
   * the remaining timeline. No-op when idle (the next start() snapshot already
   * reflects the change).
   */
  reconfigureStages(stages: RunnerConfig["stages"]): void {
    if (!this.#tickTimer || !this.#cfg) return;
    this.#cfg = { ...this.#cfg, stages };

    const { segments, totalMs } = rebuildTail(this.#segments, this.#virtualElapsed, this.#cfg);
    this.#segments = segments;
    this.#totalMs = totalMs;
    // Segment indices shifted under us — drop any armed glide so it re-arms
    // against the rebuilt array on a later tick.
    this.#glideArmedForSeg = -1;
    this.#backend.onReconfigure?.(stages);
  }

  /* ================= MASTER TICK ================= */
  #tick() {
    // Advance the virtual clock. Normally 1:1 with wall-clock; while an
    // early-finish glide is armed the position is driven directly along an eased
    // curve toward the current phase's end (§13.4).
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
      this.#exitBackendPhase(this.#lastEmittedPhase);
      const transition: PhaseTransition = {
        from: this.#lastEmittedPhase,
        to: seg.phase,
        t: elapsed,
      };
      this.#lastEmittedPhase = seg.phase;
      this.#setPhase(seg.phase);
      this.emit({ type: "phase", transition });
      this.#beginAdaptivePhase();
      this.#backend.onPhaseEnter(seg.phase, seg.warmupFor);
    }

    // Progress within the current phase (real coverage — never faked).
    const frac = (elapsed - seg.start) / (seg.end - seg.start);
    this.emit({ type: "progress", phase: seg.phase, fraction: Math.min(1, Math.max(0, frac)) });

    // Let a pull-style backend synthesize the samples due this tick. They flow
    // back in through ingestThroughput / ingestLatency before stability is read
    // below, so the just-pushed samples are in the confidence window. A push
    // (network) backend leaves onTick undefined and feeds samples on its own.
    this.#backend.onTick?.({
      phase: seg.phase,
      elapsed,
      segStart: seg.start,
      segEnd: seg.end,
      realNow: now,
    });

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
        this.emit({
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

  /* ================= SAMPLE INGEST (CoreHost) ================= */
  ingestThroughput(bytesPerSec: number, bytesDelta: number): void {
    const cfg = this.#cfg;
    if (!cfg) return;
    const phase = this.#phase;
    if (phase !== "download" && phase !== "upload") return; // ignore stray samples
    this.#bytesCumulative += bytesDelta;
    this.#accum.pushThroughput(phase, bytesPerSec, bytesDelta);
    this.emit({
      type: "throughput",
      sample: {
        t: this.#virtualElapsed,
        bytesPerSec,
        bytesCumulative: this.#bytesCumulative,
        streamCount: cfg.parallelStreams,
      },
    });
  }

  ingestLatency(rttMs: number, underLoad: boolean, lost: boolean): void {
    this.#accum.pushLatency(rttMs, underLoad, lost);
    this.emit({ type: "latency", sample: { t: this.#virtualElapsed, rttMs, underLoad, lost } });
  }

  fail(message: string): void {
    if (this.#tickTimer) {
      clearInterval(this.#tickTimer);
      this.#tickTimer = null;
    }
    this.#setPhase("error");
    this.#lastEmittedPhase = "error";
    this.emit({ type: "error", message });
  }

  /* ---------- phase helpers ---------- */
  #beginAdaptivePhase() {
    this.#glideArmedForSeg = -1; // a new phase never inherits the prior glide
    this.#accum.beginPhase();
  }

  /** Notify the backend that a (real) phase ended, so it can close I/O. Skips
   *  the non-segment phases (idle/complete/aborted/error). */
  #exitBackendPhase(phase: Phase) {
    if (phase === "warmup" || phase === "latency" || phase === "download" || phase === "upload") {
      this.#backend.onPhaseExit(phase);
    }
  }

  /** Evaluate the adaptive early-finish predicate; arm a glide if stable. */
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

  /** Fallback idle RTT for empty-sample results (the backend's hint, else 0). */
  #idleHint(): number {
    return this.#backend.idleHintMs?.() ?? 0;
  }

  /** Compute, cache, and emit a measured phase's final result exactly once —
   *  the moment it ends. No-op for warmup/non-run stages and already-finalized
   *  stages. */
  #finalizeStage(phase: Phase) {
    const cfg = this.#cfg!;
    if (phase === "download" && cfg.stages.download && !this.#dlResult) {
      this.#dlResult = this.#accum.throughputResult("download", cfg);
      this.emit({ type: "stageResult", stage: "download", result: this.#dlResult });
    } else if (phase === "upload" && cfg.stages.upload && !this.#ulResult) {
      this.#ulResult = this.#accum.throughputResult("upload", cfg);
      this.emit({ type: "stageResult", stage: "upload", result: this.#ulResult });
    } else if (phase === "latency" && cfg.stages.latency && !this.#latResult) {
      this.#latResult = this.#accum.latencyResult(cfg, this.#idleHint());
      this.emit({ type: "stageResult", stage: "latency", result: this.#latResult });
    }
  }

  /* ================= FINISH → RunResult ================= */
  #finish() {
    if (this.#tickTimer) {
      clearInterval(this.#tickTimer);
      this.#tickTimer = null;
    }

    const cfg = this.#cfg!;
    // The final measured phase ends here — finalize it like any other (the
    // earlier phases finalized at their transitions), close its I/O, then
    // assemble RunResult from the cached per-stage results so the aggregate and
    // the per-stage events never disagree.
    this.#finalizeStage(this.#lastEmittedPhase);
    this.#exitBackendPhase(this.#lastEmittedPhase);
    // Actual wall-clock length — shorter than the nominal #totalMs whenever an
    // adaptive glide accelerated one or more phases to an early finish (§13.4).
    const actualMs = Math.max(0, performance.now() - this.#t0);
    const result = {
      download: this.#dlResult,
      upload: this.#ulResult,
      // Latency is always present in the aggregate: when the latency STAGE ran it
      // was finalized as a stage result; otherwise compute it here from any
      // under-load pings (bufferbloat still needs it).
      latency: this.#latResult ?? this.#accum.latencyResult(cfg, this.#idleHint()),
      bufferbloat: this.#accum.bufferbloatGrade(this.#idleHint()),
      startedAt: Date.now() - actualMs,
      durationMs: actualMs,
    };

    this.#setPhase("complete");
    this.#lastEmittedPhase = "complete";
    this.#backend.onComplete();
    this.emit({ type: "complete", result });
  }
}

/* ---------- small helpers ---------- */
/** Smooth ease-in-out cubic on [0,1]; drives the early-finish glide curve. */
function easeInOutCubic(p: number): number {
  return p < 0.5 ? 4 * p * p * p : 1 - (-2 * p + 2) ** 3 / 2;
}
