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
 *   onRunStart → per stage: onStageBegin → onStageMeasure → onStageEnd
 *   → onComplete (or onAbort). A stage's warmup window lives BETWEEN
 *   onStageBegin and onStageMeasure, so the connection it primes is the
 *   one the measurement reuses (no cold reconnect at the seam). A
 *   pull-style backend may implement onTick to synthesize per tick; a
 *   push-style (network) backend leaves it off and calls host.ingest*
 *   from its own I/O callbacks.
 * ============================================================ */

import type {
  NetworkRunner,
  RunnerConfig,
  RunnerEvent,
  RunnerAnomaly,
  RunnerError,
  Phase,
  PhaseTransition,
  InfraInfo,
  ServerCandidate,
  ThroughputResult,
  LatencyResult,
  StallInfo,
  FlowDirection,
  TransportAttempt,
  PhaseActivity,
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

const TICK_MS = 20; // master loop resolution
const STABILITY_CADENCE_MS = 100; // ≈10Hz — pip emit rate (predicate runs every tick)

/* ---------- Stall watchdog / max-stall (§4 — measured-time model) ----------
 * The backend SHOULD bracket dead air with explicit host.stall()/resume(), but
 * a push backend can also simply go silent on a drop. The watchdog is the
 * fallback: in a MEASURED phase, if no real sample has arrived for longer than
 * STALL_WATCHDOG_MS the core auto-stalls (freezing measured-time accrual); the
 * next real sample auto-resumes. MAX_STALL_MS bounds patience — a stall that
 * outlives it is escalated to a terminal connection-lost failure rather than
 * accruing dead air forever. Both are wall-clock, never measured-time. */
const STALL_WATCHDOG_MS = 1500; // measured-phase silence → auto-stall
const MAX_STALL_MS = 20000; // stalled longer than this → terminal fail

/** Per-tick context handed to a pull-style backend so it can synthesize the
 *  samples due this tick. `realNow` lets a backend gate sample cadence on real
 *  time (e.g. during the glide, virtual time races but real time does not). */
export interface TickContext {
  phase: Segment["phase"];
  /** The active stage's resolved activity (transfer lanes + loaded latency) —
   *  the backend reads which samples to synthesize from this, never from config. */
  activity: PhaseActivity;
  /** True during the priming window (`phase === "warmup"`): a pull backend
   *  produces NO measured samples yet, matching a real backend's onStageMeasure. */
  isWarmup: boolean;
  /** absolute virtual ms since run start */
  elapsed: number;
  segStart: number;
  segEnd: number;
  /** performance.now() captured this tick */
  realNow: number;
}

/** The handle a backend uses to push raw samples / emit events into the core. */
export interface CoreHost {
  /** Push a measured throughput sample: its direction, instantaneous bytes/sec,
   *  and the bytes transferred over the interval it represents. Direction now
   *  travels WITH the sample (no phase-inference), so the bidirectional phase
   *  can carry concurrent down + up samples. The core stamps elapsed,
   *  accumulates it into the matching lane, and emits the throughput event. */
  ingestThroughput(dir: FlowDirection, bytesPerSec: number, bytesDelta: number): void;
  /** Push a measured ping: RTT, whether captured under load, and whether lost. */
  ingestLatency(rttMs: number, underLoad: boolean, lost: boolean): void;
  /** Signal a NON-terminal link stall: the link went quiet and the backend is
   *  hoping to reconnect. The core freezes measured-time accrual (so the phase
   *  end recedes by the dead-air duration) and emits a `stall` event. No-op if
   *  already stalled. Bracket with resume(); the run continues. Storing nothing
   *  in the accumulator — dead air is never a sample (principle 1). */
  stall(info: StallInfo): void;
  /** Clear a stall: measured-time accrual resumes and a `resume` event fires.
   *  No-op if not stalled. (A real sample arriving in ingest* auto-resumes too.) */
  resume(): void;
  /** Report a transport-negotiation step (which connection method, and whether
   *  it is negotiating / established / failed). The core simply re-emits it as
   *  a `transport` event — negotiation logic lives entirely in the backend. */
  reportTransport(attempt: TransportAttempt): void;
  /** Emit a raw event directly (pre-test pings during probe, connectivity, …).
   *  Bypasses accumulation — use ingest* for measured run samples. */
  emit(e: RunnerEvent): void;
  /** Report an unrecoverable failure; the core ends the run into "error",
   *  attaching the current phase + any partial results. The backend supplies
   *  the category, a human message, and (optionally) the original thrown value.
   *  `user-abort` is NOT a failure — call abort() for that. */
  fail(reason: RunnerError["reason"], message: string, cause?: unknown): void;
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
  /** A run is starting with this config. Per-stage priming happens in
   *  onStageBegin; reset any per-run state here. */
  onRunStart(config: RunnerConfig): void;
  /** A stage is beginning — the start of its warmup window (or the stage itself
   *  when warmupMs<=0). Open + PRIME every connection `activity` names (the
   *  transfer lanes, plus the ping channel when loadedLatency or a latency
   *  stage), but do NOT push measured samples yet. */
  onStageBegin(activity: PhaseActivity): void;
  /** The stage's warmup window has elapsed; the connections primed in
   *  onStageBegin are warm. START measuring on the SAME connections — never
   *  reopen them (that discards the warmup). Fires immediately after
   *  onStageBegin when the stage has no warmup window. */
  onStageMeasure(activity: PhaseActivity): void;
  /** The stage's measured window has ended (boundary, early finish, or run end).
   *  Close the stage's connection(s); the core has already finalized its result. */
  onStageEnd(activity: PhaseActivity): void;
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
  /** OPTIONAL — the measurement endpoints this backend can target (server
   *  selection seam). Single-backend engines omit it. */
  listServers?(): Promise<ServerCandidate[]>;
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
  // The segment currently driving the run, or null before the first tick / after
  // it ends. Backend stage lifecycle keys off this segment's STAGE identity (its
  // `activity.stage`), not the phase label — so the warmup→measure seam (same
  // stage) is told to MEASURE, while a stage boundary (different stage) ends the
  // old stage and begins the new one.
  #activeSeg: Segment | null = null;
  #lastStabilityAt = -Infinity;
  #bytesCumulative = 0;

  // ---- measured test-time clock + early-finish glide (§4 / §13.4) ----
  // The tick loop advances MEASURED TEST-TIME each tick. Unlike a plain
  // wall-clock it accrues ONLY while `#measuring` — so a connection drop (dead
  // air) does NOT count: the phase end recedes by the stall duration (a 5s drop
  // on a 10s budget ⇒ 15s wall-clock). Segment lookup, progress, phase-end, and
  // the glide all run off `#measuredElapsed`; the per-phase budgets are the
  // segment durations from `schedule`. At rest accrual is 1:1 with wall-clock.
  // When a measured phase becomes confidently stable an early-finish "glide" is
  // armed for that segment: measured-time is driven along an eased curve to the
  // phase's end over a short real-time window (adaptive.glideMs), so it crosses
  // seg.end — and eventually #totalMs — sooner. The progress marker visibly
  // accelerates and the run genuinely finishes early, without faking coverage.
  #measuredElapsed = 0;
  #lastRealNow = 0;
  #glideArmedForSeg = -1; // segment index a glide is armed for, or -1
  #glideStartReal = 0;
  #glideFromMeasured = 0;
  #glideTargetMeasured = 0;

  // ---- measuring / stall gate (§4 — three unbraided quantities) ----
  // `#measuring` gates measured-time accrual; it flips false on stall (explicit
  // host.stall or the watchdog) and true on resume (explicit or a real sample).
  // `#lastSampleWall` is the wall-clock of the last real sample, read by the
  // watchdog. `#stalledSinceWall` is when the current stall began (0 = not
  // stalled), read by the max-stall escalation. All wall-clock, never measured.
  #measuring = true;
  #lastSampleWall = 0;
  #stalledSinceWall = 0;
  #stallInfo: StallInfo | null = null;

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
    return this.#measuredElapsed;
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

  /** Server-selection seam: forward to the backend, or an empty list when the
   *  backend targets a single endpoint. */
  listServers(): Promise<ServerCandidate[]> {
    return this.#backend.listServers?.() ?? Promise.resolve([]);
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
    this.#lastSampleWall = this.#t0; // arm the watchdog from run start

    this.#backend.onRunStart(config);
    this.#tickTimer = setInterval(() => this.#tick(), TICK_MS);
    this.#tick(); // emit the first transition immediately
  }

  #resetRunState() {
    this.#lastEmittedPhase = "idle";
    this.#activeSeg = null;
    this.#lastStabilityAt = -Infinity;
    this.#bytesCumulative = 0;
    this.#accum.reset();
    this.#dlResult = null;
    this.#ulResult = null;
    this.#latResult = null;
    this.#measuredElapsed = 0;
    this.#lastRealNow = 0;
    this.#glideArmedForSeg = -1;
    this.#glideStartReal = 0;
    this.#glideFromMeasured = 0;
    this.#glideTargetMeasured = 0;
    this.#measuring = true;
    this.#lastSampleWall = 0;
    this.#stalledSinceWall = 0;
    this.#stallInfo = null;
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

    const { segments, totalMs } = rebuildTail(this.#segments, this.#measuredElapsed, this.#cfg);
    this.#segments = segments;
    this.#totalMs = totalMs;
    // Segment indices shifted under us — drop any armed glide so it re-arms
    // against the rebuilt array on a later tick.
    this.#glideArmedForSeg = -1;
    this.#backend.onReconfigure?.(stages);
  }

  /* ================= MASTER TICK ================= */
  #tick() {
    // Advance the MEASURED test-time clock. It accrues at the wall-rate ONLY
    // while measuring; a stall freezes it (dead air must not count), so the
    // phase end recedes by exactly the stall duration. While an early-finish
    // glide is armed the position is then driven further along an eased curve
    // toward the current phase's end (§13.4).
    const now = performance.now();
    const dtWall = now - this.#lastRealNow;
    this.#lastRealNow = now;
    if (this.#measuring) this.#measuredElapsed += dtWall;
    // The glide also advances measured-time, so it too must freeze while
    // stalled — otherwise an armed glide would race the clock through dead air.
    if (this.#measuring && this.#glideArmedForSeg >= 0) this.#advanceGlide(now);
    const elapsed = this.#measuredElapsed;

    // Stall handling runs on WALL time so it ticks even with measured-time
    // frozen. In a measured phase, prolonged silence trips the watchdog (an
    // auto-stall); a stall outliving MAX_STALL_MS escalates to a terminal fail.
    if (this.#updateStallState(now)) return; // a max-stall fail ended the run

    if (elapsed >= this.#totalMs) {
      this.#finish();
      return;
    }

    const seg = segmentAt(this.#segments, elapsed);
    if (!seg) return;

    // Segment transition? Adjacent segments always differ in phase (a warmup
    // alternates with its measured stage), so a new segment is always a phase
    // transition too — we drive both the UI phase event and the backend stage
    // lifecycle off this single edge.
    if (seg !== this.#activeSeg) {
      const prev = this.#activeSeg;
      // Same stage as the segment we're leaving ⇒ this is the warmup→measure
      // seam: keep the primed connections and just start measuring on them.
      // Otherwise we are crossing a STAGE boundary: end the old stage's I/O and
      // begin (open + prime) the new stage's.
      const sameStage = prev !== null && prev.activity.stage === seg.activity.stage;

      // The phase we're leaving has just finished — emit its final per-stage
      // result before announcing the transition, so its card resolves now; and
      // when leaving a stage entirely, close that stage's connections.
      this.#finalizeStage(this.#lastEmittedPhase);
      if (prev && !sameStage) this.#backend.onStageEnd(prev.activity);

      const transition: PhaseTransition = {
        from: this.#lastEmittedPhase,
        to: seg.phase,
        t: elapsed,
      };
      this.#activeSeg = seg;
      this.#lastEmittedPhase = seg.phase;
      this.#setPhase(seg.phase);
      this.emit({ type: "phase", transition });
      this.#beginAdaptivePhase();

      if (sameStage) {
        // Warmup window elapsed — measure on the connections already primed.
        this.#backend.onStageMeasure(seg.activity);
      } else {
        // New stage — open + prime its connection(s). When no warmup window
        // precedes it (warmupMs<=0), begin measuring immediately on the same.
        this.#backend.onStageBegin(seg.activity);
        if (seg.phase !== "warmup") this.#backend.onStageMeasure(seg.activity);
      }
    }

    // Progress within the current phase (real coverage — never faked). Fraction
    // and phaseElapsedMs are over MEASURED test-time, so both FREEZE while
    // stalled (measuredElapsed is frozen) — the bar stops and the UI's
    // budget − elapsed "time remaining" stops shrinking until resume.
    const phaseElapsedMs = elapsed - seg.start;
    const phaseBudgetMs = seg.end - seg.start;
    const frac = phaseElapsedMs / phaseBudgetMs;
    this.emit({
      type: "progress",
      phase: seg.phase,
      fraction: Math.min(1, Math.max(0, frac)),
      phaseElapsedMs,
      phaseBudgetMs,
      measuring: this.#measuring,
    });

    // Let a pull-style backend synthesize the samples due this tick. They flow
    // back in through ingestThroughput / ingestLatency before stability is read
    // below, so the just-pushed samples are in the confidence window. A push
    // (network) backend leaves onTick undefined and feeds samples on its own.
    this.#backend.onTick?.({
      phase: seg.phase,
      activity: seg.activity,
      isWarmup: seg.phase === "warmup",
      elapsed,
      segStart: seg.start,
      segEnd: seg.end,
      realNow: now,
    });

    // Stability is computed ONCE per tick for the active measured phase and
    // drives BOTH the live pip (emitted, throttled) and the early-finish glide
    // — one signal, no second meaning to reconcile (§13.4).
    if (
      seg.phase === "latency" ||
      seg.phase === "download" ||
      seg.phase === "upload" ||
      seg.phase === "bidirectional"
    ) {
      const conf: ConfidenceScore | LatencyConfidenceScore = this.#accum.confidence(seg.phase);
      // Track the trailing stable run FIRST so the emitted band reflects the
      // hysteretic latched state (entering stable takes a higher bar than
      // leaving — the pip and the stable window don't flicker). (§13.4)
      const stable = this.#accum.trackStableRun(seg.phase, conf.score, this.#cfg!.adaptive);
      // The `stability` snapshot only models the three single-lane stages; the
      // bidirectional phase still drives the early-stop glide off the combined
      // rate but emits no live pip (it has no single-lane stability signal).
      if (
        seg.phase !== "bidirectional" &&
        now - this.#lastStabilityAt >= STABILITY_CADENCE_MS
      ) {
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
      // accelerates measured-time to the segment boundary (§13.4).
      this.#maybeArmGlide(seg, elapsed, conf);
    }
  }

  /* ================= SAMPLE INGEST (CoreHost) ================= */
  ingestThroughput(dir: FlowDirection, bytesPerSec: number, bytesDelta: number): void {
    const cfg = this.#cfg;
    if (!cfg) return;
    const phase = this.#phase;
    // Direction now travels with the sample, so the bidirectional phase can
    // carry concurrent down + up samples (no phase-inference). Stray samples
    // outside a transfer/bidi phase are ignored.
    if (phase !== "download" && phase !== "upload" && phase !== "bidirectional") return;
    // A real byte sample proves delivery: refresh the watchdog + auto-resume.
    this.#noteRealSample();
    this.#bytesCumulative += bytesDelta;
    this.#accum.pushThroughput(phase, dir, bytesPerSec, bytesDelta);
    this.emit({
      type: "throughput",
      sample: {
        t: this.#measuredElapsed,
        bytesPerSec,
        bytesCumulative: this.#bytesCumulative,
        streamCount: cfg.parallelStreams,
        dir,
        phase, // narrowed to the transfer subset by the guard above
      },
    });
  }

  ingestLatency(rttMs: number, underLoad: boolean, lost: boolean): void {
    // Only a measured phase feeds the accumulator (real-stats-only): a stray
    // ping during warmup/idle must never pollute the idle-latency or bufferbloat
    // stats. (Pre-test probe pings reach the UI via host.emit, not this path.)
    const phase = this.#phase;
    if (
      phase !== "latency" &&
      phase !== "download" &&
      phase !== "upload" &&
      phase !== "bidirectional"
    ) {
      return;
    }
    // A real ping (even a lost-marked one is a real measurement event) proves
    // the link is alive: refresh the watchdog and auto-resume from any stall.
    this.#noteRealSample();
    this.#accum.pushLatency(rttMs, underLoad, lost);
    this.emit({
      type: "latency",
      sample: { t: this.#measuredElapsed, rttMs, underLoad, lost, phase: this.#phase },
    });
  }

  /* ================= STALL / RESUME (CoreHost) ================= */
  stall(info: StallInfo): void {
    // No-op if already stalled — a backend may report repeatedly, and the
    // watchdog must not re-arm a stall that's already running its max-stall
    // clock (that clock is anchored at #stalledSinceWall).
    if (!this.#measuring) return;
    this.#measuring = false;
    this.#stalledSinceWall = performance.now();
    this.#stallInfo = info;
    this.emit({ type: "stall", info });
  }

  resume(): void {
    if (this.#measuring) return; // no-op if not stalled
    this.#measuring = true;
    this.#stalledSinceWall = 0;
    this.#stallInfo = null;
    this.emit({ type: "resume" });
  }

  /** Pure pass-through: negotiation lives in the backend; the core only relays
   *  the telemetry. A terminal all-transports-failed outcome is the backend's
   *  own fail("transport-unavailable", …) call, not something we infer here. */
  reportTransport(attempt: TransportAttempt): void {
    this.emit({ type: "transport", attempt });
  }

  /** A real measured sample just arrived — refresh the watchdog and, if we were
   *  stalled, auto-resume (the link is demonstrably delivering again). Called
   *  from every ingest* path so dead-air detection keys off genuine samples. */
  #noteRealSample(): void {
    this.#lastSampleWall = performance.now();
    if (!this.#measuring) this.resume();
  }

  /** Wall-clock stall bookkeeping, run every tick. Returns true iff it ended
   *  the run (max-stall → terminal fail), so the caller bails out of the tick.
   *   • Watchdog: in a measured phase, > STALL_WATCHDOG_MS of silence while
   *     measuring → auto-stall (the backend went quiet without telling us).
   *   • Max-stall: stalled longer than MAX_STALL_MS → give up (connection-lost).
   *  Both are wall-clock; measured-time stays frozen across the whole gap. */
  #updateStallState(now: number): boolean {
    if (this.#measuring) {
      // Watchdog only inside a measured phase — warmup primes connections and
      // legitimately produces no samples, so it must not trip the watchdog.
      if (this.#isMeasuredPhase(this.#phase) && now - this.#lastSampleWall > STALL_WATCHDOG_MS) {
        this.stall({ reason: "connection-lost", detail: "no data" });
      }
      return false;
    }
    // Stalled: bound our patience. Beyond MAX_STALL_MS the drop is treated as
    // unrecoverable and escalated to a terminal failure (carries partials).
    if (now - this.#stalledSinceWall > MAX_STALL_MS) {
      // A stall never carries user-abort (that's the abort() path), but the type
      // is the broad TerminationReason; narrow to a failure reason for fail().
      const r = this.#stallInfo?.reason;
      const reason: RunnerError["reason"] = r && r !== "user-abort" ? r : "connection-lost";
      this.fail(reason, "Connection lost — gave up after max-stall timeout");
      return true;
    }
    return false;
  }

  /** Phases that produce real samples and so are subject to the stall watchdog.
   *  Warmup is excluded (it primes connections without measuring). */
  #isMeasuredPhase(phase: Phase): boolean {
    return phase === "latency" || phase === "download" || phase === "upload";
  }

  fail(reason: RunnerError["reason"], message: string, cause?: unknown): void {
    if (this.#tickTimer) {
      clearInterval(this.#tickTimer);
      this.#tickTimer = null;
    }
    const error: RunnerError = {
      reason,
      message,
      phase: this.#phase,
      partial: {
        download: this.#dlResult,
        upload: this.#ulResult,
        latency: this.#latResult,
      },
      cause,
    };
    this.#setPhase("error");
    this.#lastEmittedPhase = "error";
    this.emit({ type: "error", error });
  }

  /* ---------- phase helpers ---------- */
  #beginAdaptivePhase() {
    this.#glideArmedForSeg = -1; // a new phase never inherits the prior glide
    this.#accum.beginPhase();
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
    this.#glideFromMeasured = elapsed;
    this.#glideTargetMeasured = seg.end;
  }

  /** Drive the measured-time clock along the armed glide's eased curve. */
  #advanceGlide(now: number) {
    const glideMs = Math.max(1, this.#cfg!.adaptive.glideMs);
    const p = Math.min(1, Math.max(0, (now - this.#glideStartReal) / glideMs));
    const eased = easeInOutCubic(p);
    const target =
      this.#glideFromMeasured + (this.#glideTargetMeasured - this.#glideFromMeasured) * eased;
    // Monotonic: a jittery tick must never rewind the marker.
    this.#measuredElapsed = Math.max(this.#measuredElapsed, target);
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
    // earlier phases finalized at their transitions), end its stage (close I/O),
    // then assemble RunResult from the cached per-stage results so the aggregate
    // and the per-stage events never disagree.
    this.#finalizeStage(this.#lastEmittedPhase);
    if (this.#activeSeg) this.#backend.onStageEnd(this.#activeSeg.activity);
    // Actual wall-clock length — shorter than the nominal #totalMs whenever an
    // adaptive glide accelerated one or more phases to an early finish (§13.4),
    // and LONGER whenever a stall padded it with dead air (§4).
    const actualMs = Math.max(0, performance.now() - this.#t0);
    // Bidirectional has no per-stage event (it resolves at completion); reduce its
    // two lanes here when the stage ran, else null.
    const bidirectional = cfg.stages.bidirectional
      ? this.#accum.bidirectionalResult(cfg)
      : null;
    const result = {
      download: this.#dlResult,
      upload: this.#ulResult,
      bidirectional,
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
