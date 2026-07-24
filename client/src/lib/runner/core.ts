// Engine-agnostic runner orchestration: phase timeline, measured-time clock,
// stalls, early finish, accumulation, and RunnerEvent emission.

import type {
  NetworkRunner,
  LiveRunConfig,
  RunnerConfig,
  RunnerEvent,
  RunnerAnomaly,
  RunnerError,
  Phase,
  PhaseTransition,
  InfraInfo,
  ConnectionRole,
  EngineInfo,
  ThroughputResult,
  LatencyResult,
  StallInfo,
  FlowDirection,
  TransportAttempt,
  TransportRole,
  StageFailure,
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
  adaptiveWarmupMs,
  reconfigureTimeline,
  segmentAt,
  type Segment,
} from "./schedule";
import { RunAccumulator } from "./evaluation";
import { debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../debug";

const RUNNER_DEADLINE_MS = 100;
const STABILITY_CADENCE_MS = 100;

// Stall deadlines use wall time; result accounting retains the dead-air duration.
const STALL_WATCHDOG_MS = 1500; // measured-phase silence → auto-stall
const MAX_STALL_MS = 20000; // stalled longer than this → terminal fail

// Display and stability filter the same raw rate at different time constants.
// Exact byte and duration accounting never uses either filtered value.
const THROUGHPUT_DISPLAY_TAU_MS = 700;
const THROUGHPUT_STABILITY_TAU_MS = 1800;

type DirectionalEma = {
  down: { v: number; t: number } | null;
  up: { v: number; t: number } | null;
};

export interface CoreHost {
  // Rates drive presentation/stability; bytesDelta and durationSec remain authoritative.
  ingestThroughput(
    dir: FlowDirection,
    liveBytesPerSec: number,
    bytesDelta: number,
    durationSec: number,
    serverAuthoritative?: boolean,
  ): void;
  ingestLatency(rttMs: number, underLoad: boolean, lost: boolean): void;
  // A stall retains elapsed dead air but blocks completion until resume or timeout.
  stall(info: StallInfo): void;
  resume(): void;
  reportTransport(attempt: TransportAttempt): void;
  // Direct events bypass measurement accumulation.
  emit(e: RunnerEvent): void;
  fail(reason: RunnerError["reason"], message: string, cause?: unknown): void;
  // A stage failure skips only that stage unless no usable work remains.
  failStage(
    stage: TransportRole,
    reason: StageFailure["reason"],
    message: string,
  ): void;
  readonly config: RunnerConfig | null;
  readonly phase: Phase;
  // Virtual run time, distinct from backend wall-clock deadlines.
  readonly elapsed: number;
}

/** The stage-owned lifecycle the core drives per enabled stage, each call
 *  carrying that stage's resolved PhaseActivity. Connections belong to the
 *  STAGE, so one connection set spans its warmup and its measured window. */
export interface RunnerBackend {
  attach(host: CoreHost): void;
  // Probe may emit negative-timestamp pre-run latency events through host.emit.
  probe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo>;
  describe(): EngineInfo;
  onRunStart(config: RunnerConfig): void;
  // Open and PRIME every connection the activity names. Preparation may be
  // asynchronous; the stage clock remains parked until it resolves.
  onStageBegin(activity: PhaseActivity): void | Promise<void>;
  // Start measuring on the connections primed by onStageBegin, never reopening
  // them. Fires immediately after onStageBegin when warmupMs <= 0.
  onStageMeasure(activity: PhaseActivity): void;
  // Close the stage's connections. Result reduction waits for asynchronous
  // final samples and shutdown.
  onStageEnd(activity: PhaseActivity, flush?: boolean): void | Promise<void>;
  onComplete(): void;
  onAbort(): void;
  dispose?(): void;
  setBackgroundActivity?(enabled: boolean): void;
  onReconfigure?(stages: RunnerConfig["stages"]): void;
  idleHintMs?(): number;
  injectAnomaly?(a: RunnerAnomaly): void;
}

export class RunnerCore implements NetworkRunner, CoreHost {
  #handlers = new Set<(e: RunnerEvent) => void>();
  #phase: Phase = "idle";
  #backend: RunnerBackend;
  #cfg: RunnerConfig | null = null;

  #tickTimer: ReturnType<typeof setTimeout> | null = null;
  #running = false;
  #prepareAbort: AbortController | null = null;
  #runGeneration = 0;
  #t0 = 0; // monotonic clock reading at run start
  #segments: Segment[] = [];
  #totalMs = 0;
  #lastEmittedPhase: Phase = "idle";
  #stagePreparing = false;
  #stagePreparationId = 0;
  #activeSeg: Segment | null = null;
  #lastStabilityAt = -Infinity;
  #lastThroughputDisplayAt: Record<FlowDirection, number> = {
    down: -Infinity,
    up: -Infinity,
  };
  #lastLatencyDisplayAt = -Infinity;
  #bytesCumulative = 0;

  // Stalls remain in measured elapsed time but block finalization and adaptive glides.
  #measuredElapsed = 0;
  #lastRealNow = 0;
  #glideArmedForSeg = -1; // segment index a glide is armed for, or -1
  #glideStartReal = 0;
  #glideFromMeasured = 0;
  #glideTargetMeasured = 0;

  #measuring = true;
  #lastSampleWall = 0;
  #stalledSinceWall = 0;
  #stallInfo: StallInfo | null = null;

  #displayEma: DirectionalEma = { down: null, up: null }; // fast tau
  #stabilityEma: DirectionalEma = { down: null, up: null }; // slow tau
  #emaPhase: Phase = "idle";
  #debugThroughputLogAt: Record<FlowDirection, number> = { down: 0, up: 0 };

  #stageFailures = new Map<TransportRole, StageFailure>();

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

  probe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo> {
    return this.#backend.probe(config, signal, role);
  }

  describe(): EngineInfo {
    return this.#backend.describe();
  }

  injectAnomaly(a: RunnerAnomaly): void {
    this.#backend.injectAnomaly?.(a);
  }

  /* ================= START ================= */
  async start(config: RunnerConfig, prepared?: InfraInfo): Promise<void> {
    if (this.#running || this.#prepareAbort) this.abort();
    const generation = ++this.#runGeneration;
    const prepareAbort = new AbortController();
    this.#prepareAbort = prepareAbort;
    this.#resetRunState();

    const from = this.#phase;
    this.#phase = "connecting";
    this.#lastEmittedPhase = "connecting";
    this.emit({
      type: "phase",
      transition: { from, to: "connecting", stage: null, t: 0 },
    });

    let info = prepared;
    if (!info) {
      try {
        info = await this.probe(config, prepareAbort.signal);
      } catch (cause) {
        if (generation !== this.#runGeneration || prepareAbort.signal.aborted)
          return;
        this.#prepareAbort = null;
        throw cause;
      }
    }
    if (generation !== this.#runGeneration) return;
    this.#prepareAbort = null;
    this.emit({ type: "infra", info });

    config = {
      ...config,
      duration: {
        ...config.duration,
        warmupMs: adaptiveWarmupMs(
          config.duration.warmupMs,
          info.preTestPingMs,
        ),
      },
    };
    this.#cfg = config;

    // Build the phase timeline, skipping disabled stages (shared scheduler).
    const { segments, totalMs } = buildSegments(config);
    this.#segments = segments;
    this.#totalMs = totalMs;
    this.#t0 = performance.now();
    this.#lastRealNow = this.#t0;
    this.#lastSampleWall = this.#t0; // arm the watchdog from run start
    this.#running = true;

    this.#backend.onRunStart(config);
    this.#tick(); // emit the first transition immediately
    this.#armTick();
  }

  #resetRunState() {
    this.#running = false;
    this.#lastEmittedPhase = "idle";
    this.#activeSeg = null;
    this.#lastStabilityAt = -Infinity;
    this.#lastThroughputDisplayAt.down = -Infinity;
    this.#lastThroughputDisplayAt.up = -Infinity;
    this.#lastLatencyDisplayAt = -Infinity;
    this.#bytesCumulative = 0;
    this.#stageFailures.clear();
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
    this.#displayEma.down = this.#displayEma.up = null;
    this.#stabilityEma.down = this.#stabilityEma.up = null;
    this.#emaPhase = "idle";
    this.#stagePreparing = false;
    this.#stagePreparationId++;
  }

  /* ================= ABORT ================= */
  abort(): void {
    if (!this.#running && !this.#prepareAbort) return;
    this.#runGeneration++;
    this.#prepareAbort?.abort();
    this.#prepareAbort = null;
    if (this.#tickTimer) clearTimeout(this.#tickTimer);
    this.#tickTimer = null;
    this.#running = false;
    this.#stagePreparing = false;
    this.#stagePreparationId++;
    const from = this.#phase;
    this.#backend.onAbort();
    this.#phase = "aborted";
    this.emit({
      type: "phase",
      transition: {
        from,
        to: "aborted",
        stage: null,
        t: this.#measuredElapsed,
      },
    });
  }

  dispose(): void {
    this.#backend.dispose?.();
    this.abort();
  }

  setBackgroundActivity(enabled: boolean): void {
    this.#backend.setBackgroundActivity?.(enabled);
  }

  /* ================= LIVE RECONFIGURE ================= */
  reconfigure(config: LiveRunConfig): void {
    if (!this.#running || !this.#cfg) return;
    const activeBefore = this.#activeSeg;
    const stageNames = Object.keys(
      config.stages,
    ) as (keyof RunnerConfig["stages"])[];
    const stagesChanged = stageNames.some(
      (stage) => config.stages[stage] !== this.#cfg!.stages[stage],
    );
    this.#cfg = { ...this.#cfg, ...config };

    const { segments, totalMs } = reconfigureTimeline(
      this.#segments,
      this.#measuredElapsed,
      this.#cfg,
    );
    this.#segments = segments;
    this.#totalMs = totalMs;
    this.#glideArmedForSeg = -1;
    const activeAfter = segmentAt(segments, this.#measuredElapsed);
    if (
      activeBefore &&
      activeAfter &&
      activeAfter.phase === activeBefore.phase &&
      activeAfter.activity.stage === activeBefore.activity.stage
    ) {
      this.#activeSeg = activeAfter;
      if (activeAfter.phase !== "warmup") this.#updateStability();
    }
    if (stagesChanged) this.#backend.onReconfigure?.(config.stages);
    this.#tick();
  }

  /* ================= MASTER TICK ================= */
  #armTick(): void {
    if (!this.#tickTimer && this.#running && !this.#stagePreparing)
      this.#scheduleTick();
  }

  #scheduleTick(): void {
    const seg = segmentAt(this.#segments, this.#measuredElapsed);
    const now = performance.now();
    const deadlines = [
      RUNNER_DEADLINE_MS,
      seg ? seg.end - this.#measuredElapsed : RUNNER_DEADLINE_MS,
    ];
    if (this.#isMeasuredPhase(this.#phase)) {
      deadlines.push(
        this.#measuring
          ? this.#lastSampleWall + STALL_WATCHDOG_MS - now
          : this.#stalledSinceWall + MAX_STALL_MS - now,
      );
    }
    this.#tickTimer = setTimeout(
      () => {
        this.#tickTimer = null;
        if (!this.#running) return;
        this.#tick();
        this.#armTick();
      },
      Math.max(1, Math.min(...deadlines)),
    );
  }

  #tick() {
    // Advance measured time at wall rate so dead air remains in throughput.
    const now = performance.now();
    const dtWall = now - this.#lastRealNow;
    this.#lastRealNow = now;
    if (this.#stagePreparing) return;
    this.#measuredElapsed += dtWall;
    // Adaptive completion pauses during recovery; effective elapsed time does not.
    if (this.#measuring && this.#glideArmedForSeg >= 0) this.#advanceGlide(now);
    const elapsed = this.#measuredElapsed;

    if (elapsed >= this.#totalMs && this.#measuring) {
      this.#finish();
      return;
    }

    // Prolonged silence in a measured phase trips the watchdog into an
    // auto-stall; a stall outliving MAX_STALL_MS escalates to a terminal fail.
    if (this.#updateStallState(now)) return; // a max-stall fail ends the run

    const seg = segmentAt(this.#segments, elapsed);
    if (!seg) return;

    // Adjacent segments always differ in phase, so one segment edge drives both
    // the UI phase event and the backend stage lifecycle.
    if (seg !== this.#activeSeg) {
      const prev = this.#activeSeg;
      // Same stage ⇒ the warmup→measure seam: keep the primed connections.
      // Otherwise a STAGE boundary: end the old stage's I/O, begin the new one's.
      const sameStage =
        prev !== null && prev.activity.stage === seg.activity.stage;

      const enter = (): void => {
        this.#finalizeStage(this.#lastEmittedPhase);

        const transition: PhaseTransition = {
          from: this.#lastEmittedPhase,
          to: seg.phase,
          stage: seg.activity.stage,
          t: seg.start,
        };
        this.#activeSeg = seg;
        this.#lastEmittedPhase = seg.phase;
        this.#phase = seg.phase;
        this.emit({ type: "phase", transition });
        this.#beginAdaptivePhase();

        if (sameStage) {
          if (!this.#stageFailures.has(seg.activity.stage))
            this.#backend.onStageMeasure(seg.activity);
          return;
        }

        const preparation = this.#backend.onStageBegin(seg.activity);
        if (preparation) {
          const preparationId = ++this.#stagePreparationId;
          this.#stagePreparing = true;
          void preparation.then(
            () => {
              if (preparationId !== this.#stagePreparationId || !this.#running)
                return;
              this.#stagePreparing = false;
              this.#lastRealNow = performance.now();
              if (
                seg.phase !== "warmup" &&
                !this.#stageFailures.has(seg.activity.stage)
              )
                this.#backend.onStageMeasure(seg.activity);
              this.#tick();
              this.#armTick();
            },
            (cause) => {
              if (preparationId !== this.#stagePreparationId) return;
              this.#stagePreparing = false;
              this.fail(
                "protocol-error",
                `${seg.activity.stage} preparation failed`,
                cause,
              );
            },
          );
          this.emit({
            type: "progress",
            phase: seg.phase,
            fraction: 0,
            phaseElapsedMs: 0,
            phaseBudgetMs: seg.end - seg.start,
            measuring: true,
          });
          return;
        }
        if (
          seg.phase !== "warmup" &&
          !this.#stageFailures.has(seg.activity.stage)
        )
          this.#backend.onStageMeasure(seg.activity);
      };

      if (prev && !sameStage && this.#waitForStageEnd(prev.activity, enter))
        return;
      enter();
    }

    // Progress within the current phase: real coverage, never faked.
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
  }

  /* ================= SAMPLE INGEST (CoreHost) ================= */
  ingestThroughput(
    dir: FlowDirection,
    liveBytesPerSec: number,
    bytesDelta: number,
    durationSec: number,
    serverAuthoritative = false,
  ): void {
    const cfg = this.#cfg;
    if (!cfg) return;
    const phase = this.#phase;
    // Direction travels with the sample, so a bidirectional phase carries
    // concurrent down + up. Stray samples outside a transfer phase are ignored.
    if (phase !== "download" && phase !== "upload" && phase !== "bidirectional")
      return;
    // Zero-byte samples retain time but cannot prove delivery or clear a stall.
    if (bytesDelta > 0) this.#noteRealSample();
    this.#bytesCumulative += bytesDelta;
    // De-alias the rate twice from one raw sample: fast `display` for the UI,
    // slow `stable` for the confidence accumulator. Byte totals stay exact.
    if (this.#emaPhase !== phase) {
      this.#displayEma.down = this.#displayEma.up = null;
      this.#stabilityEma.down = this.#stabilityEma.up = null;
      this.#emaPhase = phase;
    }
    const display = this.#emaStep(
      this.#displayEma,
      dir,
      liveBytesPerSec,
      THROUGHPUT_DISPLAY_TAU_MS,
    );
    const stable = this.#emaStep(
      this.#stabilityEma,
      dir,
      liveBytesPerSec,
      THROUGHPUT_STABILITY_TAU_MS,
    );
    this.#accum.pushThroughput(
      phase,
      dir,
      stable,
      bytesDelta,
      durationSec,
      serverAuthoritative,
    );
    this.#updateStability();
    const now = performance.now();
    if (debugEnabled() && now - this.#debugThroughputLogAt[dir] >= 1000) {
      this.#debugThroughputLogAt[dir] = now;
      dlog("core:throughput", `${dir} de-alias`, {
        source: fmtRate(liveBytesPerSec),
        display: fmtRate(display),
        stable: fmtRate(stable),
        cumulative: fmtBytes(this.#bytesCumulative),
        t: fmtMs(this.#measuredElapsed),
      });
    }
    if (now - this.#lastThroughputDisplayAt[dir] >= RUNNER_DEADLINE_MS) {
      this.#lastThroughputDisplayAt[dir] = now;
      this.emit({
        type: "throughput",
        sample: {
          t: this.#measuredElapsed,
          bytesPerSec: display,
          bytesCumulative: this.#bytesCumulative,
          dir,
          phase,
        },
      });
    }
  }

  /** One dt-aware EMA step on a per-direction store. Seeded on the first sample,
   *  so no startup lag. A long stall gap grows dt and drives alpha to 1, so the
   *  first post-stall sample snaps in instead of blending with a stale value.
   *  The caller reseeds on phase change, shared across both stores. */
  #emaStep(
    store: DirectionalEma,
    dir: FlowDirection,
    raw: number,
    tau: number,
  ): number {
    const now = performance.now();
    const prev = store[dir];
    if (!prev) {
      store[dir] = { v: raw, t: now };
      return raw;
    }
    const dt = now - prev.t;
    const alpha = 1 - Math.exp(-dt / tau);
    const v = prev.v + alpha * (raw - prev.v);
    store[dir] = { v, t: now };
    return v;
  }

  ingestLatency(rttMs: number, underLoad: boolean, lost: boolean): void {
    // Only a measured phase feeds the accumulator: a stray warmup/idle ping must
    // not pollute idle-latency or bufferbloat. Probe pings use host.emit.
    const phase = this.#phase;
    if (
      phase !== "latency" &&
      phase !== "download" &&
      phase !== "upload" &&
      phase !== "bidirectional"
    ) {
      return;
    }
    // Loaded pings use another connection and cannot prove that transfer bytes
    // still move. Only the latency-only stage uses them as its liveness signal.
    if (phase === "latency") this.#noteRealSample();
    this.#accum.pushLatency(rttMs, underLoad, lost);
    if (phase === "latency") this.#updateStability();
    const now = performance.now();
    if (now - this.#lastLatencyDisplayAt >= RUNNER_DEADLINE_MS) {
      this.#lastLatencyDisplayAt = now;
      this.emit({
        type: "latency",
        sample: {
          t: this.#measuredElapsed,
          rttMs,
          underLoad,
          lost,
          phase: this.#phase,
        },
      });
    }
  }

  #updateStability(): void {
    const seg = this.#activeSeg;
    if (!seg || seg.phase === "warmup") return;
    const conf: ConfidenceScore | LatencyConfidenceScore =
      this.#accum.confidence(seg.phase);
    const stable = this.#accum.trackStableRun(
      seg.phase,
      conf.score,
      this.#cfg!.adaptive,
    );
    const now = performance.now();
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
    this.#maybeArmGlide(seg, this.#measuredElapsed, conf);
  }

  /* ================= STALL / RESUME (CoreHost) ================= */
  stall(info: StallInfo): void {
    // No-op if already stalled: a backend may report repeatedly, and re-arming
    // would reset the max-stall clock anchored at #stalledSinceWall.
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

  /** Pure pass-through: negotiation lives in the backend, the core relays the
   *  telemetry. The backend itself calls fail("transport-unavailable", …) once
   *  every transport fails. */
  reportTransport(attempt: TransportAttempt): void {
    this.emit({ type: "transport", attempt });
  }

  /** Refresh the watchdog for a real measured sample, and auto-resume a stall:
   *  the link is demonstrably delivering. Every ingest* path calls it, so
   *  dead-air detection keys off genuine samples. */
  #noteRealSample(): void {
    this.#lastSampleWall = performance.now();
    if (!this.#measuring) this.resume();
  }

  /** Wall-clock stall bookkeeping, run every tick. Returns true iff it ends the
   *  run (max-stall → terminal fail), so the caller bails out of the tick.
   *  Measured time continues across the gap. */
  #updateStallState(now: number): boolean {
    if (this.#measuring) {
      // Watchdog only inside a measured phase: warmup primes connections and
      // legitimately produces no samples.
      if (
        this.#isMeasuredPhase(this.#phase) &&
        now - this.#lastSampleWall > STALL_WATCHDOG_MS
      ) {
        this.stall({ reason: "connection-lost", detail: "no data" });
      }
      return false;
    }
    // Beyond MAX_STALL_MS the drop counts as unrecoverable: escalate to a
    // terminal failure, which carries the partial results.
    if (now - this.#stalledSinceWall > MAX_STALL_MS) {
      // A stall never carries user-abort (that's the abort() path), but the type
      // is the broad TerminationReason; narrow to a failure reason for fail().
      const stalled = this.#stallInfo?.reason;
      const reason: RunnerError["reason"] =
        stalled && stalled !== "user-abort" ? stalled : "connection-lost";
      this.fail(reason, "Connection lost — gave up after max-stall timeout");
      return true;
    }
    return false;
  }

  /** Phases subject to the stall watchdog. Warmup is excluded: it primes
   *  connections without measuring. */
  #isMeasuredPhase(phase: Phase): boolean {
    return phase === "latency" || phase === "download" || phase === "upload";
  }

  failStage(
    stage: TransportRole,
    reason: StageFailure["reason"],
    message: string,
  ): void {
    if (!this.#running || this.#stageFailures.has(stage)) return;
    const failure: StageFailure = { stage, reason, message };
    this.#stageFailures.set(stage, failure);

    // Close the stage's I/O and jump measured-time past its remaining timeline
    // so the next tick lands on the following stage (or the finish).
    if (this.#activeSeg?.activity.stage === stage) {
      this.#backend.onStageEnd(this.#activeSeg.activity, false);
      this.#activeSeg = null;
    }
    this.resume();
    this.#glideArmedForSeg = -1;
    let end = this.#measuredElapsed;
    for (const s of this.#segments)
      if (s.activity.stage === stage && s.end > end) end = s.end;
    this.#measuredElapsed = end;
    this.emit({ type: "stageSkipped", failure });

    // Nothing measured and nothing left to run → the whole run fails.
    const anyResult = this.#dlResult ?? this.#ulResult ?? this.#latResult;
    if (!anyResult && !segmentAt(this.#segments, this.#measuredElapsed)) {
      this.fail(reason, message);
    }
  }

  fail(reason: RunnerError["reason"], message: string, cause?: unknown): void {
    if (this.#phase === "error") return;
    if (this.#tickTimer) {
      clearTimeout(this.#tickTimer);
      this.#tickTimer = null;
    }
    this.#running = false;
    // Cancel the backend's in-flight I/O and let it restart its idle keepalive:
    // an errored run must not leave lanes streaming.
    this.#backend.onAbort();
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
    this.#phase = "error";
    this.#lastEmittedPhase = "error";
    this.emit({ type: "error", error });
  }

  /* ---------- phase helpers ---------- */
  #beginAdaptivePhase() {
    this.#lastStabilityAt = -Infinity;
    this.#lastLatencyDisplayAt = -Infinity;
    this.#lastThroughputDisplayAt.down = -Infinity;
    this.#lastThroughputDisplayAt.up = -Infinity;
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
    if (seg.phase === "warmup") return; // never called this way, but narrows seg.phase below
    if (this.#glideArmedForSeg >= 0) return; // already gliding this phase

    const kind: "latency" | "transfer" =
      seg.phase === "latency" ? "latency" : "transfer";
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
    // Latch the sample index where early stopping arms, so result reduction can
    // tell a fully stable phase from one destabilizing after (evaluation.ts).
    this.#accum.noteEarlyStop(seg.phase);
  }

  /** Drive the measured-time clock along the armed glide's eased curve. */
  #advanceGlide(now: number) {
    const glideMs = Math.max(1, this.#cfg!.adaptive.glideMs);
    const p = Math.min(1, Math.max(0, (now - this.#glideStartReal) / glideMs));
    const eased = easeInOutCubic(p);
    const target =
      this.#glideFromMeasured +
      (this.#glideTargetMeasured - this.#glideFromMeasured) * eased;
    // Monotonic: a jittery tick must never rewind the marker.
    this.#measuredElapsed = Math.max(this.#measuredElapsed, target);
  }

  #waitForStageEnd(activity: PhaseActivity, done: () => void): boolean {
    const ending = this.#backend.onStageEnd(activity);
    if (!ending) return false;
    const preparationId = ++this.#stagePreparationId;
    this.#stagePreparing = true;
    void ending.then(
      () => {
        if (preparationId !== this.#stagePreparationId || !this.#running)
          return;
        this.#stagePreparing = false;
        this.#lastRealNow = performance.now();
        done();
        this.#armTick();
      },
      (cause) => {
        if (preparationId !== this.#stagePreparationId) return;
        this.#stagePreparing = false;
        this.fail(
          "protocol-error",
          `${activity.stage} finalization failed`,
          cause,
        );
      },
    );
    return true;
  }

  /** Fallback idle RTT for empty-sample results (the backend's hint, else 0). */
  #idleHint(): number {
    return this.#backend.idleHintMs?.() ?? 0;
  }

  /** Compute, cache, and emit a measured phase's final result exactly once, the
   *  moment it ends. No-op for warmup, non-run, and already-finalized stages. */
  #finalizeStage(phase: Phase) {
    const cfg = this.#cfg!;
    if (this.#stageFailures.has(phase as TransportRole)) return; // skipped, no result
    if (phase === "download" && cfg.stages.download && !this.#dlResult) {
      this.#dlResult = this.#accum.throughputResult("download");
      this.emit({
        type: "stageResult",
        stage: "download",
        result: this.#dlResult,
      });
    } else if (phase === "upload" && cfg.stages.upload && !this.#ulResult) {
      this.#ulResult = this.#accum.throughputResult("upload");
      this.emit({
        type: "stageResult",
        stage: "upload",
        result: this.#ulResult,
      });
    } else if (phase === "latency" && cfg.stages.latency && !this.#latResult) {
      this.#latResult = this.#accum.latencyResult(cfg, this.#idleHint());
      this.emit({
        type: "stageResult",
        stage: "latency",
        result: this.#latResult,
      });
    }
  }

  /* ================= FINISH → RunResult ================= */
  #finish() {
    const complete = (): void => {
      if (this.#tickTimer) clearTimeout(this.#tickTimer);
      this.#tickTimer = null;
      this.#running = false;

      const cfg = this.#cfg!;
      this.#finalizeStage(this.#lastEmittedPhase);
      const actualMs = Math.max(0, performance.now() - this.#t0);
      const bidirectional =
        cfg.stages.bidirectional && !this.#stageFailures.has("bidirectional")
          ? this.#accum.bidirectionalResult()
          : null;
      const result = {
        download: this.#dlResult,
        upload: this.#ulResult,
        bidirectional,
        latency:
          this.#latResult ?? this.#accum.latencyResult(cfg, this.#idleHint()),
        bufferbloat: this.#accum.bufferbloatGrade(this.#idleHint()),
        startedAt: Date.now() - actualMs,
        durationMs: actualMs,
      };

      this.#phase = "complete";
      this.#lastEmittedPhase = "complete";
      this.#backend.onComplete();
      this.emit({ type: "complete", result });
    };

    if (
      this.#activeSeg &&
      this.#waitForStageEnd(this.#activeSeg.activity, complete)
    )
      return;
    complete();
  }
}

/* ---------- small helpers ---------- */
/** Smooth ease-in-out cubic on [0,1]; drives the early-finish glide curve. */
function easeInOutCubic(p: number): number {
  return p < 0.5 ? 4 * p * p * p : 1 - (-2 * p + 2) ** 3 / 2;
}
