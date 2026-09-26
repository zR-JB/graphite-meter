// One schedule drives every selected server; one server is the same run with one participant.
import type {
  FailureReason,
  FlowDirection,
  LatencyObservation,
  LatencyResult,
  LiveRunConfig,
  Phase,
  PhaseActivity,
  PreparedPaths,
  ReceiverCheckpoint,
  RunResult,
  RunnerConfig,
  RunnerError,
  RunnerEvent,
  StageStatus,
  StallInfo,
  ThroughputResult,
  TransportRole,
} from "./contract";
import { identity, type ServerIdentity } from "../servers/catalog";
import { ServerAuthenticationRequired } from "../servers/credentials";
import { planServerStreams, validateServerStreams } from "./paths";
import {
  EARLY_FINISH,
  pathEvidence,
  ServerLatency,
  shouldExitPhase,
  STAGES,
  ThroughputAggregate,
  type Boundary,
  type ConfidenceScore,
  type MultiServerResult,
  type ServerFailure,
  type TransferStage,
} from "./measure";
import {
  adaptiveWarmupMs,
  buildSegments,
  reconfigureTimeline,
  segmentAt,
  truncateSegmentAt,
  type Segment,
} from "./schedule";
import { LiveRates } from "./liveRates";
import { LatencyPresentationBuckets } from "./series";
import { fixedPingIntervalMs } from "./pingCadence";
import { findCause, isNetworkFailure } from "./abortable";
import {
  DIRECTION_PROGRESS_WINDOW_MS,
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  LANE_RESTART_BACKOFF_MS,
} from "./real/budgets";
import {
  ServerStage,
  type ParticipantHost,
  type StageOptions,
  type StageTransport,
} from "./transport";

export interface PreparedServer {
  server: ServerIdentity;
  paths: PreparedPaths;
}

const TICK_MS = 60;
const STABILITY_CADENCE_MS = 100;
const PROGRESS_CADENCE_MS = 250;
const TIMER_GAP_MS = 1500;
const SUMMARY_CADENCE_MS = 1000;
const LATENCY_RECOVERY_BUDGET_MS =
  2 * (ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS) + LANE_RESTART_BACKOFF_MS;

interface Participant extends PreparedServer {
  stage: StageTransport | null;
  latency: ServerLatency;
  buckets: LatencyPresentationBuckets;
  removed: boolean;
  rotated: boolean;
  /** Measured client-consumed bytes in the current stage. */
  down: number;
  /** Latest measured receiver evidence in the current stage. */
  up: ReceiverCheckpoint | null;
  progressAt: Record<FlowDirection, number>;
  recovery: {
    abort: AbortController;
    timer: ReturnType<typeof setTimeout>;
    info: StallInfo;
  } | null;
  latencyTimer: ReturnType<typeof setTimeout> | null;
  /** Ping interruptions of this server break its RTT variation pairs. */
  gaps: number;
  summaryAt: number;
}

const isTransfer = (phase: Phase): phase is TransferStage =>
  phase === "download" || phase === "upload" || phase === "bidirectional";
const isMeasured = (phase: Phase): phase is TransportRole =>
  phase === "latency" || isTransfer(phase);

const classify = <T>(cause: unknown, fallback: T) =>
  navigator.onLine === false || isNetworkFailure(cause)
    ? "connection-lost"
    : findCause(cause, DOMException)?.name === "TimeoutError"
      ? "timeout"
      : fallback;

export class Run {
  readonly #servers: Participant[];
  readonly #latencySource: Participant;
  readonly #create: (options: StageOptions) => StageTransport;
  #handlers = new Set<(event: RunnerEvent) => void>();
  #phase: Phase = "idle";
  #cfg: RunnerConfig | null = null;

  #segments: Segment[] = [];
  #active: Segment | null = null;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #running = false;
  #t0 = 0;
  #elapsed = 0;
  #lastRealNow = 0;
  /** Stage preparation or finalization holds the timeline. */
  #pending = false;
  #ending = false;
  #endRequested = false;
  /** Invalidates stage continuations after abort, finish or a newer stage. */
  #generation = 0;
  /** Invalidates in-flight evidence after membership or stage changes. */
  #epoch = 0;
  #early = { index: -1, at: 0 };
  #completedEarly = new Set<TransportRole>();
  #entered = new Set<TransportRole>();
  #progressKey = "";
  #progressAt = -Infinity;
  #stabilityAt = -Infinity;

  #activity: PhaseActivity | null = null;
  #measuring = false;
  #latencyOpen = false;
  #hasMeasured = false;
  #completed = false;
  #sampledAt = -Infinity;
  #boundaryAbort = new AbortController();
  #streams: Record<string, Record<FlowDirection, number>> = {};
  #aggregate = new ThroughputAggregate();
  #results: {
    download: ThroughputResult | null;
    upload: ThroughputResult | null;
    latency: LatencyResult | null;
  } = { download: null, upload: null, latency: null };
  #failures: ServerFailure[] = [];

  #live = new LiveRates();
  #stalled = false;
  #reported: Record<FlowDirection, number> = { down: 0, up: 0 };
  #bytes = 0;
  #continuity = 0;

  constructor(
    servers: PreparedServer[],
    latencySource: string,
    create: (options: StageOptions) => StageTransport = (options) =>
      new ServerStage(options),
  ) {
    const ids = new Set(servers.map((entry) => entry.server.id));
    if (!servers.length || servers.length > 4 || ids.size !== servers.length)
      throw new Error("Select one to four different servers");
    this.#create = create;
    this.#servers = servers.map((server) => ({
      server: identity(server.server),
      paths: server.paths,
      stage: null,
      latency: new ServerLatency(),
      buckets: new LatencyPresentationBuckets(),
      removed: false,
      rotated: false,
      down: 0,
      up: null,
      progressAt: { down: 0, up: 0 },
      recovery: null,
      latencyTimer: null,
      gaps: 0,
      summaryAt: 0,
    }));
    this.#latencySource =
      this.#servers.find((server) => server.server.id === latencySource) ??
      this.#servers[0];
  }

  get phase(): Phase {
    return this.#phase;
  }

  on(handler: (event: RunnerEvent) => void): () => void {
    this.#handlers.add(handler);
    return () => this.#handlers.delete(handler);
  }

  #emit(event: RunnerEvent): void {
    for (const handler of this.#handlers) handler(event);
  }

  #now(): number {
    return Math.max(0, performance.now() - this.#t0);
  }

  #participants(): Participant[] {
    return this.#servers.filter((server) => !server.removed);
  }

  #ids(): string[] {
    return this.#participants().map((server) => server.server.id);
  }

  #stageParticipants(activity: PhaseActivity): Participant[] {
    return this.#participants().filter(
      (server) => activity.stage !== "latency" || server.paths.latency,
    );
  }

  #validate(config: RunnerConfig, servers = this.#participants()): void {
    validateServerStreams(
      config,
      servers.map(({ server, paths }) => ({ id: server.id, paths })),
    );
  }

  /** Connection selection and authentication complete before a run starts; RTT only adjusts warmup. */
  start(config: RunnerConfig, preTestPingMs: number): void {
    this.#validate(config, this.#servers);
    this.abort();
    this.#release();
    this.#active = this.#activity = null;
    this.#elapsed = 0;
    this.#pending = this.#ending = this.#endRequested = false;
    this.#hasMeasured = this.#completed = this.#stalled = false;
    this.#completedEarly.clear();
    this.#entered.clear();
    this.#progressKey = "";
    this.#aggregate = new ThroughputAggregate();
    this.#results = { download: null, upload: null, latency: null };
    this.#failures = [];
    this.#reported = { down: 0, up: 0 };
    this.#bytes = this.#continuity = 0;
    for (const server of this.#servers)
      Object.assign(server, {
        latency: new ServerLatency(),
        removed: false,
        rotated: false,
        gaps: 0,
      });
    this.#transition("connecting", null, 0);
    const warmupMs = adaptiveWarmupMs(config.duration.warmupMs, preTestPingMs);
    this.#cfg = { ...config, duration: { ...config.duration, warmupMs } };
    this.#segments = buildSegments(this.#cfg).segments;
    this.#t0 = this.#lastRealNow = performance.now();
    this.#running = true;
    this.#tick();
    this.#arm();
  }

  abort(): void {
    if (!this.#running) return;
    this.#running = false;
    this.#flushLatency();
    this.#release();
    this.#transition("aborted", null, this.#elapsed);
  }

  dispose(): void {
    this.abort();
    this.#release();
    this.#handlers.clear();
  }

  /** End after the active stage with every retained result; later stages do not run. */
  finish(): void {
    if (!this.#running) return;
    this.#endRequested = true;
    if (this.#ending) return;
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    this.#generation++;
    this.#pending = false;
    if (this.#active)
      void this.#endStage(this.#active.activity, () => this.#complete());
    else this.#complete();
  }

  reconfigure(config: LiveRunConfig): void {
    if (!this.#running || !this.#cfg) return;
    const next = { ...this.#cfg, ...config };
    this.#validate(next);
    const before = this.#active;
    this.#cfg = next;
    this.#segments = reconfigureTimeline(
      this.#segments,
      this.#elapsed,
      next,
    ).segments;
    const after = segmentAt(this.#segments, this.#elapsed);
    if (
      before &&
      after?.phase === before.phase &&
      after.activity.stage === before.activity.stage
    ) {
      this.#active = after;
      for (const server of this.#servers)
        server.buckets.widen(after.end - after.start);
      if (after.phase !== "warmup") this.#updateStability();
    }
    this.#tick();
  }

  #release(): void {
    this.#epoch++;
    this.#generation++;
    this.#measuring = this.#latencyOpen = false;
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    this.#boundaryAbort.abort();
    this.#boundaryAbort = new AbortController();
    this.#aggregate.close();
    for (const server of this.#servers) {
      this.#cancelRecovery(server);
      this.#cancelLatencyTimer(server);
      server.stage?.discard();
      server.stage = null;
    }
  }

  #transition(to: Phase, stage: TransportRole | null, t: number): void {
    this.#phase = to;
    this.#emit({ type: "phase", transition: { to, stage, t } });
  }

  #arm(): void {
    if (this.#timer || !this.#running || this.#pending) return;
    const deadlines = [TICK_MS];
    const segment = segmentAt(this.#segments, this.#elapsed);
    if (segment) deadlines.push(segment.end - this.#elapsed);
    for (const server of this.#servers) {
      const boundary = server.buckets.nextBoundaryT;
      if (boundary != null) deadlines.push(boundary - this.#elapsed);
    }
    this.#timer = setTimeout(
      () => {
        this.#timer = null;
        if (!this.#running) return;
        this.#tick();
        this.#arm();
      },
      Math.max(1, Math.min(...deadlines)),
    );
  }

  #tick(): void {
    const now = performance.now();
    const dtWall = now - this.#lastRealNow;
    this.#lastRealNow = now;
    if (this.#pending) return;
    // One tick never crosses the current segment's end, so a suspended page still enters every segment in order.
    const current = segmentAt(this.#segments, this.#elapsed);
    this.#elapsed = Math.min(
      this.#elapsed + Math.max(0, dtWall),
      current?.end ?? Infinity,
    );
    if (dtWall > TIMER_GAP_MS && isMeasured(this.#phase)) this.#resetInterval();
    for (const server of this.#servers)
      for (const sample of server.buckets.closeThrough(this.#elapsed))
        this.#emit({
          type: "serverLatency",
          serverId: server.server.id,
          sample,
        });
    if (this.#measuring && now - this.#sampledAt >= TICK_MS - 1)
      this.#sample(now);
    if (this.#elapsed >= (this.#segments.at(-1)?.end ?? 0) && !this.#stalled)
      return this.finish();
    const segment = segmentAt(this.#segments, this.#elapsed);
    if (!segment) return;
    if (this.#early.index >= 0 && this.#updateStability()) return this.#tick();
    if (segment !== this.#active) this.#enter(segment);
    const phaseElapsedMs = this.#elapsed - segment.start;
    const phaseBudgetMs = segment.end - segment.start;
    const key = `${segment.phase}:${phaseBudgetMs}:${!this.#stalled}`;
    if (
      key === this.#progressKey &&
      now - this.#progressAt < PROGRESS_CADENCE_MS
    )
      return;
    this.#progressKey = key;
    this.#progressAt = now;
    this.#emit({
      type: "progress",
      phase: segment.phase,
      fraction: Math.min(1, Math.max(0, phaseElapsedMs / phaseBudgetMs)),
      phaseElapsedMs,
      phaseBudgetMs,
      measuring: !this.#stalled,
    });
  }

  /** Adjacent segments always differ in phase; a warmup and its measurement share one stage. */
  #enter(segment: Segment): void {
    const previous = this.#active;
    const sameStage = previous?.activity.stage === segment.activity.stage;
    const enter = () => {
      this.#finalize(this.#phase);
      this.#active = segment;
      const show = () =>
        this.#transition(segment.phase, segment.activity.stage, segment.start);
      this.#cancelEarly();
      this.#stabilityAt = -Infinity;
      this.#continuity++;
      if (sameStage) {
        show();
        return this.#measureStage();
      }
      const generation = ++this.#generation;
      this.#pending = true;
      // The first stage keeps showing connection checks; later ones show no countdown until ready.
      if (previous) {
        show();
        this.#emit({
          type: "progress",
          phase: segment.phase,
          fraction: 0,
          phaseElapsedMs: 0,
          phaseBudgetMs: 0,
          measuring: true,
        });
      }
      this.#beginStage(segment.activity).then(
        () => {
          if (generation !== this.#generation || !this.#running) return;
          this.#pending = false;
          this.#lastRealNow = performance.now();
          if (!previous) show();
          if (segment.phase !== "warmup") this.#measureStage();
          this.#tick();
          this.#arm();
        },
        (cause) => {
          if (generation !== this.#generation) return;
          this.#pending = false;
          this.#fail(
            classify(cause, "protocol-error"),
            cause instanceof Error ? cause.message : "Stage preparation failed",
          );
        },
      );
    };
    if (previous && !sameStage) void this.#endStage(previous.activity, enter);
    else enter();
  }

  async #beginStage(activity: PhaseActivity): Promise<void> {
    this.#activity = activity;
    this.#measuring = this.#latencyOpen = false;
    const epoch = ++this.#epoch;
    this.#entered.add(activity.stage);
    if (this.#servers.length === 1) this.#servers[0].removed = false;
    const participants = this.#stageParticipants(activity);
    this.#streams = planServerStreams(
      this.#cfg!,
      this.#participants().map(({ server, paths }) => ({
        id: server.id,
        paths,
      })),
      activity,
    );
    for (const server of this.#participants()) {
      server.down = 0;
      server.up = null;
      server.latency.failed.delete(activity.stage);
    }
    const seed = `r${Math.round(this.#t0)}`;
    const results = await Promise.allSettled(
      participants.map(async (server) => {
        const streams = this.#streams[server.server.id];
        const stage = this.#create({
          host: this.#host(server),
          paths: server.paths,
          activity,
          streams,
          seed,
        });
        server.stage = stage;
        await stage.prepare();
        await stage.ready(this.#boundaryAbort.signal);
      }),
    );
    if (epoch !== this.#epoch) return;
    for (const [index, result] of results.entries())
      if (result.status === "rejected")
        this.#remove(
          participants[index],
          classify(result.reason, "preparation-failed"),
          result.reason instanceof Error
            ? result.reason.message
            : "Measurement preparation failed",
        );
  }

  #measureStage(): void {
    const activity = this.#activity!;
    const cfg = this.#cfg!;
    if (!this.#participants().length) return;
    this.#hasMeasured = this.#measuring = this.#latencyOpen = true;
    const cadence =
      activity.stage === "latency" ? cfg.pingCadence : cfg.loadedPingCadence;
    for (const server of this.#stageParticipants(activity)) {
      const span = (this.#active?.end ?? 0) - (this.#active?.start ?? 0);
      server.buckets.reset(
        this.#elapsed,
        activity.stage,
        activity.stage !== "latency",
        this.#continuity,
        span,
        fixedPingIntervalMs(cadence),
      );
      if (activity.stage === "latency") server.latency.resetStability();
      const now = performance.now();
      server.progressAt = { down: now, up: now };
      server.stage?.measure();
    }
    if (!isTransfer(activity.stage)) return;
    this.#live.reset(Object.fromEntries(this.#ids().map((id) => [id, 0])));
    this.#aggregate.begin(activity.stage, this.#ids(), this.#now());
    this.#boundary();
  }

  #snapshot(): Boundary {
    const participants = this.#participants();
    return {
      atMs: this.#now(),
      down: Object.fromEntries(
        participants.map((server) => [server.server.id, server.down]),
      ),
      up: Object.fromEntries(
        participants.map((server) => [server.server.id, server.up]),
      ),
    };
  }

  /** Samples every participant's current evidence; true when adaptive completion was confirmed. */
  #boundary(): boolean {
    if (!this.#measuring || !this.#activity?.transfer.length || this.#completed)
      return false;
    return this.#observe(this.#snapshot());
  }

  #observe(boundary: Boundary, final = false): boolean {
    const interval = this.#aggregate.current?.id;
    const sample = this.#aggregate.observe(boundary);
    if (interval !== this.#aggregate.current?.id) this.#resetStability();
    if (!sample) return false;
    for (const dir of ["down", "up"] as const) {
      const total = this.#servers.reduce(
        (sum, server) => sum + this.#aggregate.totals(server.server.id)[dir],
        0,
      );
      this.#bytes += Math.max(0, total - this.#reported[dir]);
      this.#reported[dir] = total;
    }
    return !final && !this.#stalled && this.#updateStability();
  }

  /** The presentation cadence: download boundaries and live rates, which results never read. */
  #sample(now: number): void {
    this.#sampledAt = now;
    const transfer = this.#activity?.transfer ?? [];
    if (transfer.includes("down")) this.#boundary();
    const phase = this.#phase;
    if (!isTransfer(phase)) return;
    if (!this.#stalled)
      for (const server of this.#participants())
        this.#live.download(server.server.id, server.down, now);
    const rate = (dir: FlowDirection) =>
      !transfer.includes(dir) ? null : this.#stalled ? 0 : this.#live.rate(dir);
    const lanes = (id: string) => {
      const server = this.#servers.find((entry) => entry.server.id === id)!;
      return server.paths.throughput.target.transport === "fetch-stream"
        ? (this.#streams[id]?.up ?? 0)
        : 1;
    };
    const elapsed =
      this.#elapsed +
      (this.#pending ? 0 : Math.max(0, now - this.#lastRealNow));
    this.#emit({
      type: "live",
      sample: {
        t: Math.min(elapsed, this.#active?.end ?? elapsed),
        phase,
        continuityId: this.#continuity,
        bytes: this.#bytes,
        down: rate("down"),
        up: rate("up"),
        bridgedUp:
          this.#stalled || !transfer.includes("up")
            ? null
            : this.#live.bridgedUpload(now, lanes),
        stalled: this.#stalled,
      },
    });
  }

  /** Each participant stops as soon as its own final evidence is known. */
  async #endStage(activity: PhaseActivity, then: () => void): Promise<void> {
    const generation = this.#generation;
    this.#pending = this.#ending = true;
    const ending = new Map<Participant, Promise<unknown>>();
    const end = (server: Participant) => {
      if (!ending.has(server) && server.stage)
        ending.set(
          server,
          server.stage.finish().catch(() => {}),
        );
    };
    if (this.#measuring && activity.transfer.length)
      await this.#finalBoundary(end);
    this.#measuring = false;
    // Evidence that stopped in this stage fails this stage, not the next one.
    for (const server of this.#participants())
      if (server.recovery || server.latencyTimer)
        this.#remove(
          server,
          server.recovery?.info.reason ?? "connection-lost",
          server.recovery?.info.detail ??
            "Server stopped delivering measured data",
        );
    for (const server of this.#stageParticipants(activity)) end(server);
    await Promise.all(ending.values());
    if (generation !== this.#generation) return;
    this.#latencyOpen = false;
    for (const server of this.#servers) {
      this.#flushLatency(server);
      const summary = server.latency.stages[activity.stage].summary();
      this.#emit({
        type: "serverLatencySummary",
        serverId: server.server.id,
        stage: activity.stage,
        summary,
      });
      this.#cancelRecovery(server);
      this.#cancelLatencyTimer(server);
      server.stage = null;
    }
    this.#emit({ type: "serverDetails", details: this.details() });
    this.#pending = this.#ending = false;
    this.#lastRealNow = performance.now();
    if (this.#endRequested) this.#complete();
    else then();
    this.#arm();
  }

  /** Terminal evidence after this boundary cannot change measured totals or rates. */
  async #finalBoundary(settled: (server: Participant) => void): Promise<void> {
    const epoch = this.#epoch;
    const participants = this.#participants();
    const upload = this.#activity!.transfer.includes("up");
    const boundary = this.#snapshot();
    this.#measuring = false;
    const results = await Promise.allSettled(
      participants.map((server) =>
        (upload && server.stage
          ? server.stage.checkpoint(this.#boundaryAbort.signal, true)
          : Promise.resolve(null)
        ).finally(() => settled(server)),
      ),
    );
    if (epoch !== this.#epoch || this.#completed) return;
    if (upload)
      participants.forEach((server, index) => {
        const result = results[index];
        boundary.up[server.server.id] =
          result.status === "fulfilled" ? result.value : null;
      });
    this.#observe(boundary, true);
    this.#aggregate.close();
    for (const [index, result] of results.entries())
      if (
        result.status === "rejected" &&
        result.reason instanceof ServerAuthenticationRequired
      )
        this.#remove(
          participants[index],
          "sign-in-required",
          result.reason.message,
        );
  }

  #host(server: Participant): ParticipantHost {
    const run = this;
    const id = server.server.id;
    const live = () => !server.removed && !!run.#activity;
    return {
      get config() {
        return run.#cfg!;
      },
      now: () => run.#now(),
      download(bytes) {
        const stage = run.#activity?.stage;
        if (
          !run.#measuring ||
          !live() ||
          !stage ||
          !isTransfer(stage) ||
          !(bytes > 0)
        )
          return;
        server.down += bytes;
        server.progressAt.down = performance.now();
        run.#aggregate.addDownload(stage, id, bytes);
      },
      receiver(checkpoint) {
        if (!run.#measuring || !live()) return;
        if (checkpoint.bytes > (server.up?.bytes ?? -1))
          server.progressAt.up = performance.now();
        server.up = checkpoint;
        if (!run.#stalled)
          run.#live.receiver(id, checkpoint, performance.now());
        if (run.#boundary()) run.#tick();
      },
      latency: (sample) => run.#latency(server, sample),
      latencyInterrupted(count, reason) {
        if (run.#latencyOpen && run.#activity)
          server.latency.stages[run.#activity.stage].interrupt(count, reason);
      },
      latencyIncomplete() {
        if (!run.#latencyOpen || !run.#activity) return;
        server.latency.stages[run.#activity.stage].markIncomplete();
        run.#failure(
          server,
          "latency",
          "connection-lost",
          "Latency observations were interrupted",
        );
      },
      stall: (info) => run.#stall(server, info),
      resume() {
        if (server.recovery) run.#live.restart(id, server.down);
        run.#cancelRecovery(server);
        run.#cancelLatencyTimer(server);
        run.#updateStalled();
      },
      stallLatency(detail) {
        server.gaps++;
        run.#stallLatency(server, detail);
      },
      resumeLatency() {
        server.gaps++;
        run.#cancelLatencyTimer(server);
        run.#updateStalled();
      },
      fail: (reason, message) => run.#remove(server, reason, message),
      authenticationRequired(role) {
        const message = `Sign in to ${server.server.name}`;
        // Loaded latency alone never removes a throughput participant.
        if (role === "throughput" || run.#activity?.stage === "latency")
          run.#remove(server, "sign-in-required", message);
        else run.#failure(server, "latency", "sign-in-required", message);
      },
      uploadHint(lane, bytes, elapsedMs) {
        if (live())
          run.#live.hint(id, lane, bytes, elapsedMs, performance.now());
      },
    };
  }

  /** Translates a window-clock observation using the last timeline tick, not its delivery time. */
  #observationTime(observedAtMs: number): number {
    const projected =
      this.#elapsed + Math.max(0, performance.now() - this.#lastRealNow);
    return Math.max(
      this.#active?.start ?? 0,
      Math.min(projected, this.#elapsed + observedAtMs - this.#lastRealNow),
    );
  }

  #latency(server: Participant, sample: LatencyObservation): void {
    if (!this.#latencyOpen || server.removed || !this.#activity) return;
    const stage = this.#activity.stage;
    const t = this.#observationTime(sample.observedAtMs);
    const id = server.server.id;
    server.latency.observe(stage, sample, t, server.gaps);
    if (sample.rttEligible !== false)
      for (const bucket of server.buckets.observe(
        t,
        sample.rttMs,
        sample.timedOut,
      ))
        this.#emit({ type: "serverLatency", serverId: id, sample: bucket });
    const now = performance.now();
    if (now - server.summaryAt >= SUMMARY_CADENCE_MS) {
      server.summaryAt = now;
      const summary = server.latency.stages[stage].summary();
      this.#emit({
        type: "serverLatencySummary",
        serverId: id,
        stage,
        summary,
      });
    }
    if (
      stage === "latency" &&
      now - this.#stabilityAt >= STABILITY_CADENCE_MS &&
      this.#updateStability()
    )
      this.#tick();
  }

  #flushLatency(only?: Participant): void {
    for (const server of only ? [only] : this.#servers) {
      const sample = server.buckets.flush(this.#elapsed);
      if (sample)
        this.#emit({
          type: "serverLatency",
          serverId: server.server.id,
          sample,
        });
    }
  }

  #stall(server: Participant, info: StallInfo): void {
    const activity = this.#activity;
    if (server.removed || server.recovery || !activity) return;
    if (activity.stage === "latency")
      return this.#stallLatency(server, info.detail ?? "Latency interrupted");
    const abort = new AbortController();
    const now = performance.now();
    // Silence across a page timer gap is the page's, not the server's.
    const quietMs =
      now - this.#lastRealNow > TIMER_GAP_MS
        ? 0
        : now - server.progressAt[info.direction ?? activity.transfer[0]];
    const timer = setTimeout(
      () => {
        if (server.recovery?.abort === abort)
          this.#remove(
            server,
            info.reason,
            info.detail ?? "Server stopped delivering measured data",
          );
      },
      Math.max(0, DIRECTION_PROGRESS_WINDOW_MS - quietMs),
    );
    server.recovery = { abort, timer, info };
    this.#live.restart(server.server.id, server.down);
    this.#cancelEarly();
    this.#resetStability();
    this.#updateStalled();
    // An unknown upload id grants one replacement receiver per server and run.
    if (info.rotate && info.direction === "up" && !server.rotated) {
      server.rotated = true;
      void server.stage?.replaceUpload?.(abort.signal);
    }
  }

  #stallLatency(server: Participant, detail: string): void {
    const stage = this.#activity?.stage;
    if (
      server.removed ||
      !stage ||
      server.latency.failed.has(stage) ||
      server.latencyTimer
    )
      return;
    server.latencyTimer = setTimeout(() => {
      server.latencyTimer = null;
      server.latency.failed.add(stage);
      server.latency.stages[stage].markIncomplete();
      this.#failure(server, "latency", "connection-lost", detail);
      this.#updateStalled();
    }, LATENCY_RECOVERY_BUDGET_MS);
    this.#updateStalled();
  }

  #cancelRecovery(server: Participant): void {
    if (!server.recovery) return;
    clearTimeout(server.recovery.timer);
    server.recovery.abort.abort();
    server.recovery = null;
  }

  #cancelLatencyTimer(server: Participant): void {
    if (server.latencyTimer) clearTimeout(server.latencyTimer);
    server.latencyTimer = null;
  }

  /** The run is stalled while every participant is recovering its measured evidence. */
  #updateStalled(): void {
    const latencyStage = this.#activity?.stage === "latency";
    const servers = latencyStage
      ? this.#latencyParticipants()
      : this.#participants();
    const recovering = (server: Participant) =>
      !!server.recovery || (latencyStage && !!server.latencyTimer);
    const stalled = servers.length > 0 && servers.every(recovering);
    if (stalled === this.#stalled || !this.#running) return;
    this.#stalled = stalled;
    this.#breakContinuity();
    if (!stalled) {
      this.#live.reset(this.#counts());
      return this.#emit({ type: "resume" });
    }
    const info = servers.find((server) => server.recovery)?.recovery?.info;
    this.#emit({
      type: "stall",
      info: info ?? {
        reason: "connection-lost",
        detail: "Latency interrupted",
      },
    });
  }

  #failure(
    server: Participant,
    scope: ServerFailure["scope"],
    reason: FailureReason,
    message: string,
  ): void {
    const failure = this.#record(server.server.id, scope, reason, message);
    if (failure)
      this.#emit({ type: "serverFailure", failure, participants: this.#ids() });
  }

  #record(
    serverId: string,
    scope: ServerFailure["scope"],
    reason: FailureReason,
    message: string,
    stage = this.#activity?.stage,
  ): ServerFailure | undefined {
    if (
      !stage ||
      this.#failures.some(
        (old) =>
          old.serverId === serverId &&
          old.stage === stage &&
          old.scope === scope,
      )
    )
      return;
    // Server-supplied detail is bounded before it can reach saved history.
    const failure: ServerFailure = {
      serverId,
      stage,
      atMs: this.#now(),
      scope,
      reason,
      message: message.slice(0, 256),
    };
    this.#failures.push(failure);
    return failure;
  }

  #remove(server: Participant, reason: FailureReason, message: string): void {
    if (server.removed || this.#completed || !this.#running) return;
    const activity = this.#activity;
    if (activity?.stage === "latency") {
      server.latency.failed.add("latency");
      server.latency.stages.latency.markIncomplete();
      this.#cancelLatencyTimer(server);
      this.#failure(server, "latency", reason, message);
      this.#updateStalled();
      if (!this.#latencyParticipants().length) this.#skipStage();
      return;
    }
    server.removed = true;
    this.#cancelRecovery(server);
    this.#cancelLatencyTimer(server);
    // Failed-stage shutdown accounts for buffered probes before the worker is terminated.
    server.stage?.discard(true);
    server.stage = null;
    this.#live.drop(server.server.id);
    this.#failure(server, "throughput", reason, message);
    this.#epoch++;
    const survivors = this.#ids();
    this.#updateStalled();
    // A sole server skips to its next stage; several that all fail end the run as incomplete.
    if (!survivors.length && this.#hasMeasured)
      return this.#servers.length === 1 ? this.#skipStage() : this.finish();
    if (!survivors.length)
      return this.#fail(
        reason,
        `All selected servers failed. ${server.server.name}: ${message}`,
      );
    // Survivors start a fresh fixed-membership interval.
    if (activity && this.#measuring && isTransfer(activity.stage))
      this.#aggregate.begin(activity.stage, survivors, this.#now(), "dropout");
    this.#cancelEarly();
    this.#resetStability();
    if (this.#boundary()) this.#tick();
  }

  /** With nobody left to measure, the stage ends now and keeps any result its evidence supports. */
  #skipStage(): void {
    const stage = this.#activity?.stage;
    if (!stage || this.#ending) return;
    for (const segment of this.#segments)
      if (segment.activity.stage === stage)
        this.#elapsed = Math.max(this.#elapsed, segment.end);
    this.#cancelEarly();
    queueMicrotask(() => {
      if (this.#running) this.#tick();
    });
  }

  #counts(): Record<string, number> {
    return Object.fromEntries(
      this.#participants().map((server) => [server.server.id, server.down]),
    );
  }

  /** A timer gap starts a new interval, so no headline, early finish or live rate spans it. */
  #resetInterval(): void {
    this.#cancelEarly();
    this.#resetStability();
    this.#live.reset(this.#counts());
    this.#breakContinuity();
    const stage = this.#activity?.stage;
    if (!this.#measuring || !stage || !isTransfer(stage)) return;
    for (const server of this.#participants()) server.up = null;
    this.#aggregate.begin(stage, this.#ids(), this.#now(), "evidence-resumed");
    this.#boundary();
  }

  #resetStability(): void {
    this.#aggregate.resetStability();
    if (this.#activity?.stage === "latency")
      for (const server of this.#servers) server.latency.resetStability();
  }

  /** Ends every presentation series at a lifecycle boundary. */
  #breakContinuity(): void {
    this.#flushLatency();
    this.#continuity++;
    for (const server of this.#servers)
      server.buckets.restart(this.#elapsed, this.#continuity);
  }

  #latencyParticipants(): Participant[] {
    return this.#participants().filter(
      (server) => server.paths.latency && !server.latency.failed.has("latency"),
    );
  }

  /** Completion respects the least stable and least sampled path without pooling populations. */
  #confidence(phase: TransportRole): ConfidenceScore {
    if (phase !== "latency") return this.#aggregate.confidence();
    const scores = this.#latencyParticipants().map((server) =>
      server.latency.confidence(),
    );
    return scores.length
      ? {
          score: Math.min(...scores.map((s) => s.score)),
          sampleCount: Math.min(...scores.map((s) => s.sampleCount)),
        }
      : { score: 0, sampleCount: 0 };
  }

  #updateStability(): boolean {
    const segment = this.#active;
    if (!segment || segment.phase === "warmup") return false;
    const confidence = this.#confidence(segment.phase);
    if (segment.phase === "latency")
      for (const server of this.#latencyParticipants())
        server.latency.trackStable(confidence.score);
    else this.#aggregate.trackStable(confidence.score);
    this.#stabilityAt = performance.now();
    return this.#updateEarly(segment, segment.phase, confidence);
  }

  #canComplete(phase: TransportRole): boolean {
    if (phase !== "latency")
      return (
        this.#aggregate.sufficient &&
        this.#participants().every((server) => !server.recovery)
      );
    const servers = this.#latencyParticipants();
    return (
      servers.length > 0 && servers.every((server) => !server.latencyTimer)
    );
  }

  #cancelEarly(): void {
    this.#early = { index: -1, at: 0 };
  }

  /** Arms, revokes or confirms an early finish without changing measured time. */
  #updateEarly(
    segment: Segment,
    phase: TransportRole,
    confidence: ConfidenceScore,
  ): boolean {
    const cfg = this.#cfg!;
    const eligible =
      cfg.adaptive &&
      !this.#stalled &&
      this.#canComplete(phase) &&
      shouldExitPhase({
        kind: phase === "latency" ? "latency" : "transfer",
        cadence: cfg.pingCadence,
        elapsedMs: this.#elapsed - segment.start,
        durationMs: segment.end - segment.start,
        confidence,
      });
    if (!eligible) {
      this.#cancelEarly();
      return false;
    }
    const index = this.#segments.indexOf(segment);
    if (this.#early.index !== index) this.#early = { index, at: this.#elapsed };
    if (this.#elapsed - this.#early.at < EARLY_FINISH.confirmationMs)
      return false;
    const total = this.#segments.at(-1)?.end ?? 0;
    this.#segments = truncateSegmentAt(
      this.#segments,
      segment,
      this.#elapsed,
    ).segments;
    if ((this.#segments.at(-1)?.end ?? 0) < total)
      this.#completedEarly.add(phase);
    this.#early = { index: -1, at: 0 };
    return true;
  }

  /** Each measured stage's result is reduced and emitted once, the moment it ends. */
  #finalize(phase: Phase): void {
    const cfg = this.#cfg!;
    if (phase === "latency" && cfg.stages.latency && !this.#results.latency) {
      const result = this.#latencySource.latency.result();
      this.#results.latency = result;
      if (result) this.#emit({ type: "stageResult", stage: "latency", result });
    }
    if (
      (phase !== "download" && phase !== "upload") ||
      !cfg.stages[phase] ||
      this.#results[phase]
    )
      return;
    const result = this.#aggregate.result(
      phase,
      this.#completedEarly.has(phase),
    )[phase === "download" ? "down" : "up"];
    this.#results[phase] = result;
    if (result) this.#emit({ type: "stageResult", stage: phase, result });
  }

  #status(stage: TransportRole, lanes: unknown[]): StageStatus {
    const cfg = this.#cfg!;
    if (!cfg.stages[stage] || !(cfg.duration[`${stage}Ms`] > 0))
      return "not-run";
    const scope = stage === "latency" ? "latency" : "throughput";
    const failed = () =>
      this.#failures.some(
        (failure) => failure.stage === stage && failure.scope === scope,
      );
    if (lanes.every(Boolean)) return failed() ? "partial" : "complete";
    if (!this.#entered.has(stage) || failed()) return "failed";
    const ids =
      stage === "latency"
        ? [this.#latencySource.server.id]
        : (this.#aggregate.intervals.findLast(
            (interval) => interval.stage === stage,
          )?.participants ?? this.#ids());
    for (const id of ids)
      this.#record(
        id,
        scope,
        "insufficient-evidence",
        "Too little measured evidence for a result",
        stage,
      );
    return "failed";
  }

  #complete(): void {
    this.#running = false;
    this.#completed = true;
    const cfg = this.#cfg!;
    this.#finalize(this.#phase);
    const durationMs = Math.max(0, performance.now() - this.#t0);
    const source = this.#latencySource.latency;
    const bidirectional = cfg.stages.bidirectional
      ? this.#aggregate.result(
          "bidirectional",
          this.#completedEarly.has("bidirectional"),
        )
      : null;
    const lanes = {
      ...this.#results,
      bidirectional: bidirectional && [bidirectional.down, bidirectional.up],
    };
    const stages = Object.fromEntries(
      STAGES.map((stage) => [
        stage,
        this.#status(stage, [lanes[stage]].flat()),
      ]),
    ) as RunResult["stages"];
    const statuses = Object.values(stages);
    const result: RunResult = {
      ...this.#results,
      bidirectional,
      stages,
      bufferbloat: source.bufferbloat(),
      latencyByStage: source.summaries(),
      multiServer: this.details(),
      outcome: statuses.includes("failed")
        ? "incomplete"
        : this.#failures.length
          ? "partial"
          : "complete",
      startedAt: Date.now() - durationMs,
      durationMs,
    };
    this.#release();
    for (const server of this.#servers) server.latency.close();
    this.#phase = "complete";
    this.#emit({ type: "complete", result });
  }

  #fail(reason: RunnerError["reason"], message: string): void {
    if (this.#phase === "error") return;
    this.#running = false;
    this.#flushLatency();
    this.#release();
    this.#phase = "error";
    this.#emit({ type: "error", error: { reason, message } });
  }

  details(): MultiServerResult {
    const cfg = this.#cfg;
    const aggregate = this.#aggregate;
    return {
      selection: this.#servers.map((server) => server.server),
      participants: this.#ids(),
      latencyFocus: this.#latencySource.server.id,
      intervals: structuredClone(aggregate.intervals),
      omittedIntervals: aggregate.omittedIntervals,
      failures: [...this.#failures],
      servers: this.#servers.map(({ server, paths, latency }) => {
        const result = (stage: TransferStage, dir: FlowDirection) =>
          aggregate.serverResult(stage, dir, server.id);
        return {
          server,
          ...pathEvidence(paths),
          latency: latency.result(),
          latencyByStage: latency.summaries(),
          bufferbloat: latency.bufferbloat(),
          download: result("download", "down"),
          upload: result("upload", "up"),
          bidirectional: cfg?.stages.bidirectional
            ? {
                down: result("bidirectional", "down"),
                up: result("bidirectional", "up"),
              }
            : null,
          totalBytes: aggregate.totals(server.id),
        };
      }),
    };
  }
}
