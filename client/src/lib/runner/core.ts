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
  RecoveryCause,
  TransportRole,
  StageFailure,
  PhaseActivity,
  LatencyObservation,
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
  truncateSegmentAt,
  type Segment,
} from "./schedule";
import { RunAccumulator } from "./evaluation";
import { debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../debug";
import { GrowingRateEstimator } from "./rateEstimator";
import { LatencyPresentationBuckets } from "./latencyBuckets";
import {
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  LANE_RESTART_BACKOFF_MS,
} from "./real/budgets";

// This bounds only runner publication/progress work. Authoritative sources keep
// their own cadence; the core never creates an observation to fill this slot.
const PRESENTATION_CADENCE_MS = 60;
const RUNNER_DEADLINE_MS = PRESENTATION_CADENCE_MS;
const STABILITY_CADENCE_MS = 100;

// Stall deadlines use wall time; result accounting retains the dead-air duration.
const STALL_WATCHDOG_MS = 1500; // measured-phase silence → auto-stall
const RECOVERY_ATTEMPTS = 2;
const RECOVERY_ATTEMPT_MS = ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS;
export const STAGE_RECOVERY_BUDGET_MS =
  RECOVERY_ATTEMPTS * RECOVERY_ATTEMPT_MS +
  (RECOVERY_ATTEMPTS - 1) * LANE_RESTART_BACKOFF_MS;

export interface CoreHost {
  // Rates drive presentation/stability; bytesDelta and durationSec remain authoritative.
  ingestThroughput(
    dir: FlowDirection,
    liveBytesPerSec: number,
    bytesDelta: number,
    durationSec: number,
    serverAuthoritative?: boolean,
    /** Whether this sample proves every required direction is healthy. */
    provesLiveness?: boolean,
  ): void;
  ingestLatency(observation: LatencyObservation, underLoad: boolean): void;
  /** Account a rotation handoff in final byte/time reduction only. It is never
   * a source observation, presentation sample, or control/confidence input. */
  recordRecoveryGap(dir: FlowDirection, durationSec: number): void;
  /** Account bytes first visible at a replacement upload checkpoint. Their
   * server count is authoritative, but it has no prior checkpoint from which
   * to derive a live rate, so it stays out of presentation and control. */
  recordRecoveryBytes(dir: FlowDirection, bytes: number): void;
  // A stall retains elapsed dead air but blocks completion until resume or timeout.
  stall(info: StallInfo): void;
  resume(): void;
  // Direct events bypass measurement accumulation.
  emit(e: RunnerEvent): void;
  fail(reason: RunnerError["reason"], message: string, cause?: unknown): void;
  // A stage failure skips only that stage unless no usable work remains.
  failStage(
    stage: TransportRole,
    reason: StageFailure["reason"],
    message: string,
    direction?: FlowDirection,
  ): void;
  readonly config: RunnerConfig | null;
  readonly phase: Phase;
  // Virtual run time, distinct from backend wall-clock deadlines.
  readonly elapsed: number;
  /** Current authoritative estimator output for a visual-only bridge. */
  presentationRate(dir: FlowDirection): number;
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
  /** The runner owns recovery expiry. Backends may make bounded attempts only
   * while this signal remains live; opening a transport is not recovery. */
  onStageRecovery?(request: {
    stage: TransportRole;
    direction?: FlowDirection;
    cause: RecoveryCause;
    signal: AbortSignal;
  }): void | Promise<void>;
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
  #bytesCumulative = 0;

  // Stalls remain in measured elapsed time but block adaptive confirmation.
  #measuredElapsed = 0;
  #lastRealNow = 0;
  #earlyCandidateSeg = -1;
  #earlyCandidateStartedAt = 0;

  #measuring = true;
  #lastSampleWall = 0;
  #stalledSinceWall = 0;
  #recoveryDeadlineWall = 0;
  #recoveryAbort: AbortController | null = null;
  #stallInfo: StallInfo | null = null;
  #stallPresentationStartedAt = 0;
  #stallPresentationFrom: Record<FlowDirection, number> = { down: 0, up: 0 };

  #rateEstimator: Record<FlowDirection, GrowingRateEstimator> = {
    down: new GrowingRateEstimator(),
    up: new GrowingRateEstimator(),
  };
  #presentedRate: Record<FlowDirection, number> = { down: 0, up: 0 };
  #continuityId = 0;
  #latencyBuckets = new LatencyPresentationBuckets();
  #debugThroughputLogAt: Record<FlowDirection, number> = { down: 0, up: 0 };

  #stageFailures = new Map<TransportRole, StageFailure>();
  #completedEarlyStages = new Set<TransportRole>();

  #accum = new RunAccumulator();
  #dlResult: ThroughputResult | null = null;
  #ulResult: ThroughputResult | null = null;
  #latResult: LatencyResult | null = null;
  #biResult: {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
  } | null = null;

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
  /** `prepared` is the InfraInfo an earlier probe() resolved. The app always
   *  passes one, since validateConnections probes before it starts a run; a
   *  caller that has not probed omits it and start resolves the selection
   *  itself, inside the `connecting` phase so the wait stays cancellable. */
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
    this.#bytesCumulative = 0;
    this.#stageFailures.clear();
    this.#completedEarlyStages.clear();
    this.#accum.reset();
    this.#dlResult = null;
    this.#ulResult = null;
    this.#latResult = null;
    this.#biResult = null;
    this.#measuredElapsed = 0;
    this.#lastRealNow = 0;
    this.#earlyCandidateSeg = -1;
    this.#earlyCandidateStartedAt = 0;
    this.#measuring = true;
    this.#lastSampleWall = 0;
    this.#stalledSinceWall = 0;
    this.#recoveryDeadlineWall = 0;
    this.#recoveryAbort?.abort();
    this.#recoveryAbort = null;
    this.#stallInfo = null;
    this.#stallPresentationStartedAt = 0;
    this.#stallPresentationFrom = { down: 0, up: 0 };
    this.#rateEstimator.down.reset();
    this.#rateEstimator.up.reset();
    this.#presentedRate = { down: 0, up: 0 };
    this.#continuityId = 0;
    this.#stagePreparing = false;
    this.#stagePreparationId++;
  }

  /* ================= ABORT ================= */
  abort(): void {
    if (!this.#running && !this.#prepareAbort) return;
    this.#runGeneration++;
    this.#prepareAbort?.abort();
    this.#prepareAbort = null;
    this.#recoveryAbort?.abort();
    this.#recoveryAbort = null;
    if (this.#tickTimer) clearTimeout(this.#tickTimer);
    this.#tickTimer = null;
    this.#running = false;
    this.#stagePreparing = false;
    this.#stagePreparationId++;
    const from = this.#phase;
    this.#flushLatencyPresentation();
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
    const activeAfter = segmentAt(segments, this.#measuredElapsed);
    if (
      activeBefore &&
      activeAfter &&
      activeAfter.phase === activeBefore.phase &&
      activeAfter.activity.stage === activeBefore.activity.stage
    ) {
      this.#activeSeg = activeAfter;
      this.#latencyBuckets.widen(activeAfter.end - activeAfter.start);
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
    const latencyBoundary = this.#latencyBuckets.nextBoundaryT;
    if (latencyBoundary != null)
      deadlines.push(latencyBoundary - this.#measuredElapsed);
    if (this.#isMeasuredPhase(this.#phase)) {
      deadlines.push(
        this.#measuring
          ? this.#lastSampleWall + STALL_WATCHDOG_MS - now
          : this.#recoveryDeadlineWall - now,
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
    const elapsed = this.#measuredElapsed;
    this.#emitClosedLatencyPresentation();

    if (elapsed >= this.#totalMs && this.#measuring) {
      this.#finish();
      return;
    }

    // Prolonged silence trips the watchdog; the runner alone expires recovery.
    if (this.#updateStallState(now)) return;
    if (!this.#measuring) this.#emitStallPresentation(now);

    const seg = segmentAt(this.#segments, elapsed);
    if (!seg) return;

    // Confirmation time advances normally even when no new source callback is
    // due. Confidence itself changes only when exact observations arrive.
    if (this.#earlyCandidateSeg >= 0 && this.#updateStability()) {
      this.#tick();
      return;
    }

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
            this.#startStageMeasure(seg.activity);
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
                this.#startStageMeasure(seg.activity);
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
          this.#startStageMeasure(seg.activity);
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

  /** A measured stage receives a fresh silence budget. Preparation and warmup
   * deliberately produce no transfer evidence, so neither may inherit the
   * previous stage's watchdog deadline. */
  #startStageMeasure(activity: PhaseActivity): void {
    this.#lastSampleWall = performance.now();
    this.#backend.onStageMeasure(activity);
  }

  /* ================= SAMPLE INGEST (CoreHost) ================= */
  ingestThroughput(
    dir: FlowDirection,
    liveBytesPerSec: number,
    bytesDelta: number,
    durationSec: number,
    serverAuthoritative = false,
    provesLiveness = true,
  ): void {
    const cfg = this.#cfg;
    if (!cfg) return;
    const phase = this.#phase;
    // Direction travels with the sample, so a bidirectional phase carries
    // concurrent down + up. Stray samples outside a transfer phase are ignored.
    if (phase !== "download" && phase !== "upload" && phase !== "bidirectional")
      return;
    // Zero-byte samples retain time but cannot prove delivery or clear a stall.
    if (bytesDelta > 0 && provesLiveness) this.#noteRealSample();
    this.#bytesCumulative += bytesDelta;
    this.#accum.pushThroughput(
      phase,
      dir,
      liveBytesPerSec,
      bytesDelta,
      durationSec,
      serverAuthoritative,
    );
    // A stalled direction may keep reporting accounting windows, and a healthy
    // bidirectional sibling may keep moving without proving stage liveness.
    // Preserve both in the exact reducer, but leave the presentation estimator
    // parked so only the centralized stall transition can drive live rates.
    if (!this.#measuring) return;
    const estimate = this.#rateEstimator[dir].observe({
      bytes: bytesDelta,
      durationMs: durationSec * 1_000,
    });
    this.#presentedRate[dir] = estimate.presentedBytesPerSec;
    if (estimate.regimeChanged) {
      this.#accum.resetPhaseStability(phase);
      this.#cancelEarlyCandidate();
    }
    if (this.#updateStability()) this.#tick();
    const now = performance.now();
    if (debugEnabled() && now - this.#debugThroughputLogAt[dir] >= 1000) {
      this.#debugThroughputLogAt[dir] = now;
      dlog("core:throughput", `${dir} exact windows`, {
        source: fmtRate(liveBytesPerSec),
        presented: fmtRate(estimate.presentedBytesPerSec),
        fast: fmtRate(estimate.fastBytesPerSec),
        regime: estimate.regimeId,
        candidate: estimate.candidate ?? "none",
        cumulative: fmtBytes(this.#bytesCumulative),
        t: fmtMs(this.#measuredElapsed),
      });
    }
    if (
      estimate.regimeChanged ||
      now - this.#lastThroughputDisplayAt[dir] >= PRESENTATION_CADENCE_MS
    ) {
      this.#lastThroughputDisplayAt[dir] = now;
      this.#emitThroughputPresentation(dir, estimate.presentedBytesPerSec);
    }
  }

  recordRecoveryGap(dir: FlowDirection, durationSec: number): void {
    if (durationSec <= 0) return;
    const phase = this.#phase;
    if (phase !== "download" && phase !== "upload" && phase !== "bidirectional")
      return;
    this.#accum.recordRecoveryGap(phase, dir, durationSec);
  }

  recordRecoveryBytes(dir: FlowDirection, bytes: number): void {
    if (bytes <= 0) return;
    const phase = this.#phase;
    if (phase !== "download" && phase !== "upload" && phase !== "bidirectional")
      return;
    this.#bytesCumulative += bytes;
    this.#accum.recordRecoveryBytes(phase, dir, bytes);
  }

  presentationRate(dir: FlowDirection): number {
    return this.#presentedRate[dir];
  }

  #emitThroughputPresentation(dir: FlowDirection, bytesPerSec: number): void {
    const phase = this.#phase;
    if (phase !== "download" && phase !== "upload" && phase !== "bidirectional")
      return;
    this.#presentedRate[dir] = bytesPerSec;
    this.emit({
      type: "throughput",
      sample: {
        t: this.#measuredElapsed,
        bytesPerSec,
        bytesCumulative: this.#bytesCumulative,
        dir,
        phase,
        continuityId: this.#continuityId,
      },
    });
  }

  ingestLatency(observation: LatencyObservation, underLoad: boolean): void {
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
    // Translate the authoritative wall observation into the measured timeline.
    // Projection avoids assigning the runner's up-to-100 ms tick lag to the
    // sample, while phase and receipt bounds reject stale or future timestamps.
    const wallNow = performance.now();
    const projectedNow =
      this.#measuredElapsed + Math.max(0, wallNow - this.#lastRealNow);
    const observedT = Math.max(
      this.#activeSeg?.start ?? 0,
      Math.min(
        projectedNow,
        this.#measuredElapsed + observation.observedAtMs - this.#lastRealNow,
      ),
    );
    this.#accum.pushLatency(
      observation.rttMs,
      underLoad,
      observation.lost,
      observedT,
    );
    if (phase === "latency" && this.#updateStability()) this.#tick();
    for (const bucket of this.#latencyBuckets.observe(
      observedT,
      observation.rttMs,
      observation.lost,
    ))
      this.emit({
        type: "latency",
        sample: bucket,
      });
  }

  #flushLatencyPresentation(): void {
    const bucket = this.#latencyBuckets.flush(this.#measuredElapsed);
    if (bucket) this.emit({ type: "latency", sample: bucket });
  }

  #emitClosedLatencyPresentation(): void {
    for (const bucket of this.#latencyBuckets.closeThrough(
      this.#measuredElapsed,
    ))
      this.emit({ type: "latency", sample: bucket });
  }

  /** End every presentation series at a lifecycle boundary and seed latency's
   *  next bucket with the same continuity generation as throughput. */
  #breakPresentationContinuity(): void {
    this.#flushLatencyPresentation();
    this.#continuityId++;
    this.#resetLatencyPresentation();
  }

  #resetLatencyPresentation(): void {
    this.#latencyBuckets.reset(
      this.#measuredElapsed,
      this.#phase,
      this.#phase !== "latency",
      this.#continuityId,
      Math.max(0, (this.#activeSeg?.end ?? 0) - (this.#activeSeg?.start ?? 0)),
    );
  }

  #updateStability(): boolean {
    const seg = this.#activeSeg;
    if (!seg || seg.phase === "warmup") return false;
    const conf: ConfidenceScore | LatencyConfidenceScore =
      this.#accum.confidence(seg.phase);
    const stable = this.#accum.trackStableRun(
      seg.phase,
      conf.score,
      this.#cfg!.adaptive,
    );
    const now = performance.now();
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
    return this.#updateEarlyCandidate(seg, this.#measuredElapsed, conf);
  }

  /* ================= STALL / RESUME (CoreHost) ================= */
  stall(info: StallInfo): void {
    // No-op if already stalled: a backend may report repeatedly, and re-arming
    // would reset the max-stall clock anchored at #stalledSinceWall.
    if (!this.#measuring) return;
    this.#measuring = false;
    this.#stalledSinceWall = performance.now();
    this.#recoveryDeadlineWall =
      this.#stalledSinceWall + STAGE_RECOVERY_BUDGET_MS;
    this.#recoveryAbort?.abort();
    this.#recoveryAbort = new AbortController();
    this.#stallPresentationStartedAt = this.#stalledSinceWall;
    this.#stallPresentationFrom = { ...this.#presentedRate };
    this.#stallInfo = info;
    this.#breakPresentationContinuity();
    this.#cancelEarlyCandidate();
    if (this.#activeSeg && this.#activeSeg.phase !== "warmup")
      this.#accum.resetPhaseStability(this.#activeSeg.phase);
    this.#rateEstimator.down.invalidateRegime();
    this.#rateEstimator.up.invalidateRegime();
    this.emit({ type: "stall", info });
    const stage = this.#activeSeg?.activity.stage;
    const cause = info.recoveryCause ?? "transient-connection";
    if (stage)
      void this.#backend.onStageRecovery?.({
        stage,
        direction: info.direction,
        cause,
        signal: this.#recoveryAbort.signal,
      });
  }

  resume(): void {
    if (this.#measuring) return; // no-op if not stalled
    this.#measuring = true;
    this.#stalledSinceWall = 0;
    this.#recoveryDeadlineWall = 0;
    this.#recoveryAbort?.abort();
    this.#recoveryAbort = null;
    this.#stallInfo = null;
    this.#stallPresentationStartedAt = 0;
    this.#breakPresentationContinuity();
    this.emit({ type: "resume" });
  }

  #emitStallPresentation(now: number): void {
    if (!this.#stallPresentationStartedAt) return;
    const phase = this.#phase;
    const dirs: FlowDirection[] =
      phase === "download"
        ? ["down"]
        : phase === "upload"
          ? ["up"]
          : phase === "bidirectional"
            ? ["down", "up"]
            : [];
    const elapsed = now - this.#stallPresentationStartedAt;
    for (const dir of dirs) {
      const rate = GrowingRateEstimator.stallRate(
        this.#stallPresentationFrom[dir],
        elapsed,
      );
      if (rate !== this.#presentedRate[dir] || elapsed === 0)
        this.#emitThroughputPresentation(dir, rate);
    }
  }

  /** Refresh the watchdog for a real measured sample, and auto-resume a stall:
   *  the link is demonstrably delivering. Every ingest* path calls it, so
   *  dead-air detection keys off genuine samples. */
  #noteRealSample(): void {
    this.#lastSampleWall = performance.now();
    if (!this.#measuring) this.resume();
  }

  /** Wall-clock stall bookkeeping. The watchdog detects silence; only the
   * runner's derived recovery deadline may finalize the affected stage. */
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
    if (now >= this.#recoveryDeadlineWall) {
      const stalled = this.#stallInfo?.reason;
      const reason: StageFailure["reason"] =
        stalled && stalled !== "user-abort" ? stalled : "connection-lost";
      const stage = this.#activeSeg?.activity.stage;
      if (stage)
        this.failStage(
          stage,
          reason,
          "Connection lost — recovery deadline expired",
        );
      else this.fail(reason, "Connection lost — recovery deadline expired");
      return true;
    }
    return false;
  }

  /** Phases subject to the stall watchdog. Warmup is excluded: it primes
   *  connections without measuring. */
  #isMeasuredPhase(phase: Phase): boolean {
    return (
      phase === "latency" ||
      phase === "download" ||
      phase === "upload" ||
      phase === "bidirectional"
    );
  }

  failStage(
    stage: TransportRole,
    reason: StageFailure["reason"],
    message: string,
    direction?: FlowDirection,
  ): void {
    if (!this.#running || this.#stageFailures.has(stage)) return;
    const failure: StageFailure = { stage, direction, reason, message };
    this.#stageFailures.set(stage, failure);
    // Preserve qualifying exact evidence before ending the stage. A failure
    // changes its status to partial; it does not erase a truthful result.
    this.#finalizeStage(stage);

    // Close the stage's I/O and jump measured-time past its remaining timeline
    // so the next tick lands on the following stage (or the finish).
    if (this.#activeSeg?.activity.stage === stage) {
      this.#backend.onStageEnd(this.#activeSeg.activity, false);
      this.#activeSeg = null;
    }
    this.resume();
    this.#cancelEarlyCandidate();
    let end = this.#measuredElapsed;
    for (const s of this.#segments)
      if (s.activity.stage === stage && s.end > end) end = s.end;
    this.#measuredElapsed = end;
    this.emit({ type: "stageSkipped", failure });

    // Nothing measured and nothing left to run → the whole run fails.
    const anyResult =
      this.#dlResult ??
      this.#ulResult ??
      this.#latResult ??
      this.#biResult?.down ??
      this.#biResult?.up;
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
    this.#flushLatencyPresentation();
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
        bidirectional: this.#biResult,
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
    this.#lastThroughputDisplayAt.down = -Infinity;
    this.#lastThroughputDisplayAt.up = -Infinity;
    this.#cancelEarlyCandidate();
    this.#rateEstimator.down.reset();
    this.#rateEstimator.up.reset();
    this.#presentedRate = { down: 0, up: 0 };
    this.#continuityId++;
    this.#accum.beginPhase();
    this.#resetLatencyPresentation();
  }

  #hasRegimeCandidate(seg: Segment): boolean {
    if (seg.phase === "download")
      return this.#rateEstimator.down.snapshot().candidate !== null;
    if (seg.phase === "upload")
      return this.#rateEstimator.up.snapshot().candidate !== null;
    if (seg.phase === "bidirectional")
      return (
        this.#rateEstimator.down.snapshot().candidate !== null ||
        this.#rateEstimator.up.snapshot().candidate !== null
      );
    return false;
  }

  #cancelEarlyCandidate(): void {
    this.#earlyCandidateSeg = -1;
    this.#earlyCandidateStartedAt = 0;
    this.#accum.cancelLatencyEarlyStop();
  }

  /** Arm, revoke, or confirm an early finish without changing measured time. */
  #updateEarlyCandidate(
    seg: Segment,
    elapsed: number,
    conf: ConfidenceScore | LatencyConfidenceScore,
  ): boolean {
    const cfg = this.#cfg!;
    if (seg.phase === "warmup") return false;

    const evidencePolicy =
      seg.phase === "latency"
        ? ({ kind: "latency", latencyCadence: cfg.pingCadence } as const)
        : ({ kind: "transfer" } as const);
    const eligible =
      this.#measuring &&
      !this.#hasRegimeCandidate(seg) &&
      shouldExitPhase({
        ...evidencePolicy,
        elapsedMs: elapsed - seg.start,
        durationMs: seg.end - seg.start,
        confidence: conf,
        cfg: cfg.adaptive,
      });
    if (!eligible) {
      this.#cancelEarlyCandidate();
      return false;
    }

    const segIndex = this.#segments.indexOf(seg);
    if (this.#earlyCandidateSeg !== segIndex) {
      this.#earlyCandidateSeg = segIndex;
      this.#earlyCandidateStartedAt = elapsed;
      if (seg.phase === "latency") this.#accum.armLatencyEarlyStop();
    }
    if (elapsed - this.#earlyCandidateStartedAt < cfg.adaptive.confirmationMs)
      return false;

    if (seg.phase === "latency") this.#accum.confirmLatencyEarlyStop();
    const previousTotalMs = this.#totalMs;
    const truncated = truncateSegmentAt(this.#segments, seg, elapsed);
    this.#segments = truncated.segments;
    this.#totalMs = truncated.totalMs;
    if (truncated.totalMs < previousTotalMs)
      this.#completedEarlyStages.add(seg.activity.stage);
    this.#earlyCandidateSeg = -1;
    this.#earlyCandidateStartedAt = 0;
    return true;
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
    this.#flushLatencyPresentation();
    const failed = this.#stageFailures.has(phase as TransportRole);
    if (phase === "download" && cfg.stages.download && !this.#dlResult) {
      this.#dlResult = failed
        ? this.#accum.partialThroughputResult("download")
        : this.#accum.throughputResult(
            "download",
            this.#completedEarlyStages.has("download"),
          );
      if (this.#dlResult)
        this.emit({
          type: "stageResult",
          stage: "download",
          result: this.#dlResult,
        });
    } else if (phase === "upload" && cfg.stages.upload && !this.#ulResult) {
      this.#ulResult = failed
        ? this.#accum.partialThroughputResult("upload")
        : this.#accum.throughputResult(
            "upload",
            this.#completedEarlyStages.has("upload"),
          );
      if (this.#ulResult)
        this.emit({
          type: "stageResult",
          stage: "upload",
          result: this.#ulResult,
        });
    } else if (phase === "latency" && cfg.stages.latency && !this.#latResult) {
      this.#latResult = failed
        ? this.#accum.partialLatencyResult(cfg, this.#idleHint())
        : this.#accum.latencyResult(cfg, this.#idleHint());
      if (this.#latResult)
        this.emit({
          type: "stageResult",
          stage: "latency",
          result: this.#latResult,
        });
    } else if (
      phase === "bidirectional" &&
      cfg.stages.bidirectional &&
      failed
    ) {
      this.#biResult ??= this.#accum.partialBidirectionalResult();
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
      const bidirectional = cfg.stages.bidirectional
        ? this.#stageFailures.has("bidirectional")
          ? (this.#biResult ?? this.#accum.partialBidirectionalResult())
          : this.#accum.bidirectionalResult(
              this.#completedEarlyStages.has("bidirectional"),
            )
        : null;
      const result = {
        download: this.#dlResult,
        upload: this.#ulResult,
        bidirectional,
        latency: this.#latResult,
        bufferbloat: this.#accum.bufferbloatGrade(),
        stageFailures: Object.fromEntries(this.#stageFailures),
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
