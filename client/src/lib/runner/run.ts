// One schedule drives every selected server; one server is the same run with one participant.
import type {
  FailureReason,
  FlowDirection,
  LatencyObservation,
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
  TransportRole,
} from "./contract";
import { safeDetail } from "../api/decode";
import { identity, type ServerIdentity } from "../servers/catalog";
import { ServerAuthenticationRequired } from "../servers/credentials";
import { planServerStreams, validatePlan } from "./paths";
import { combineCompensationEstimates, wireModel } from "../compensation";
import { headlineWire } from "../servers/wireEstimates";
import {
  EARLY_FINISH,
  pathEvidence,
  ServerLatency,
  shouldExitPhase,
  ThroughputAggregate,
  type Boundary,
  type ConfidenceScore,
  type MultiServerResult,
  type ServerFailure,
  type TransferStage,
} from "./measure";
import {
  buildSegments,
  planned,
  reconfigureTimeline,
  segmentAt,
  STAGES,
  truncateSegmentAt,
  type Segment,
} from "./schedule";
import { RunClock } from "./clock";
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
  ServerBusyError,
  ServerStage,
  type ParticipantHost,
  type StageOptions,
  type StageTransport,
} from "./transport";

export interface PreparedServer {
  server: ServerIdentity;
  paths: PreparedPaths;
}
export interface DroppedServer {
  server: ServerIdentity;
  reason: FailureReason;
  message: string;
}

const TICK_MS = 60;
const STABILITY_CADENCE_MS = 100;
const PROGRESS_CADENCE_MS = 250;
const STALL_QUIET_MS = 500;
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
  /** Run-clock active time of the last measured progress in each direction. */
  progressAt: Record<FlowDirection, number>;
  recovery: { abort: AbortController; info: StallInfo } | null;
  latencyStall: { at: number; detail: string } | null;
  /** Ping interruptions of this server break its RTT variation pairs. */
  gaps: number;
  summaryAt: number;
}

const isTransfer = (phase: Phase): phase is TransferStage =>
  phase === "download" || phase === "upload" || phase === "bidirectional";
const isMeasured = (phase: Phase): phase is TransportRole =>
  phase === "latency" || isTransfer(phase);

type StageEvidence = Pick<
  RunResult,
  "latency" | "download" | "upload" | "bidirectional"
>;
const noEvidence = (): StageEvidence => ({
  latency: null,
  download: null,
  upload: null,
  bidirectional: null,
});
const stageLanes = (evidence: StageEvidence, stage: TransportRole) =>
  stage === "bidirectional"
    ? [evidence.bidirectional?.down, evidence.bidirectional?.up]
    : [evidence[stage]];
/** The run and each server share one rule: every lane, then no failure. */
const stageStatus = (lanes: unknown[], failed: boolean): StageStatus =>
  !lanes.every(Boolean) ? "failed" : failed ? "partial" : "complete";

const classify = <T>(cause: unknown, fallback: T) =>
  findCause(cause, ServerBusyError)
    ? "server-busy"
    : navigator.onLine === false || isNetworkFailure(cause)
      ? "connection-lost"
      : findCause(cause, DOMException)?.name === "TimeoutError"
        ? "timeout"
        : fallback;

export class Run {
  readonly #servers: Participant[];
  readonly #latencySource: Participant;
  readonly #create: (options: StageOptions) => StageTransport;
  readonly #dropped: DroppedServer[];
  #handlers = new Set<(event: RunnerEvent) => void>();
  #phase: Phase = "idle";
  #cfg: RunnerConfig | null = null;

  #segments: Segment[] = [];
  #active: Segment | null = null;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #running = false;
  #clock = new RunClock();
  #elapsed = 0;
  #tickAt = 0;
  #ending = false;
  #endRequested = false;
  /** Invalidates stage continuations after abort, finish or a newer stage. */
  #generation = 0;
  /** Invalidates in-flight stage outcomes after release or a newer stage; a removal leaves the others' outcomes valid. */
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
  #results: StageEvidence = noEvidence();
  #settled: Partial<Record<TransportRole, StageStatus>> = {};
  #failures: ServerFailure[] = [];

  #live = new LiveRates();
  #stalled = false;
  #reported: Record<FlowDirection, number> = { down: 0, up: 0 };
  #bytes = 0;
  #continuity = 0;

  constructor(
    servers: PreparedServer[],
    latencySource: string,
    dropped: DroppedServer[] = [],
    create: (options: StageOptions) => StageTransport = (options) =>
      new ServerStage(options),
  ) {
    this.#dropped = dropped;
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
      latencyStall: null,
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
    return this.#clock.read() - this.#clock.start;
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

  /** Connection selection, authentication and the RTT-adapted plan are settled before a run starts. */
  start(config: RunnerConfig): void {
    validatePlan(config, this.#servers);
    this.abort();
    this.#release();
    this.#active = this.#activity = null;
    this.#elapsed = 0;
    this.#ending = this.#endRequested = false;
    this.#hasMeasured = this.#completed = this.#stalled = false;
    this.#completedEarly.clear();
    this.#entered.clear();
    this.#progressKey = "";
    this.#aggregate = new ThroughputAggregate();
    this.#results = noEvidence();
    this.#settled = {};
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
    this.#clock = new RunClock();
    this.#tickAt = this.#clock.start;
    this.#transition("connecting", null, 0);
    this.#cfg = config;
    this.#segments = buildSegments(config).segments;
    // A server dropped before the run fails the first stage it would have carried bytes in.
    const first = (
      this.#segments.find(({ activity }) => activity.transfer.length) ??
      this.#segments[0]
    )?.activity.stage;
    for (const { server, reason, message } of this.#dropped)
      this.#record(server.id, "throughput", reason, message, first);
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

  /** Ends now with every retained result; each stage left unfinished fails for `reason`. */
  end(
    reason: FailureReason,
    message: string,
    servers: readonly PreparedServer[] = this.#participants(),
  ): void {
    if (!this.#running) return;
    for (const stage of STAGES)
      if (planned(this.#cfg!, stage) && !this.#settled[stage])
        for (const { server } of servers)
          this.#record(
            server.id,
            stage === "latency" ? "latency" : "throughput",
            reason,
            message,
            stage,
          );
    this.finish();
  }

  /** End after the active stage with every retained result; later stages do not run. */
  finish(): void {
    if (!this.#running) return;
    this.#endRequested = true;
    if (this.#ending) return;
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    this.#generation++;
    if (this.#active)
      void this.#endStage(this.#active.activity, () => this.#complete());
    else this.#complete();
  }

  reconfigure(config: LiveRunConfig): void {
    if (!this.#running || !this.#cfg) return;
    const next = { ...this.#cfg, ...config };
    validatePlan(next, this.#participants());
    const before = this.#active;
    this.#cfg = next;
    this.#segments = reconfigureTimeline(
      this.#segments,
      this.#elapsed,
      next,
      this.#ending,
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
      server.latencyStall = null;
      server.stage?.discard();
      server.stage = null;
      server.latency.close();
    }
  }

  #transition(to: Phase, stage: TransportRole | null, t: number): void {
    this.#phase = to;
    this.#emit({ type: "phase", transition: { to, stage, t } });
  }

  #arm(): void {
    if (this.#timer || !this.#running || this.#clock.held) return;
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
    const now = this.#clock.read();
    const dtWall = now - this.#tickAt;
    this.#tickAt = now;
    if (this.#clock.held) return;
    // One tick never crosses the current segment's end, so a suspended page still enters every segment in order.
    const current = segmentAt(this.#segments, this.#elapsed);
    this.#elapsed = Math.min(
      this.#elapsed + Math.max(0, dtWall),
      current?.end ?? Infinity,
    );
    if (this.#clock.takeGap() && isMeasured(this.#phase)) this.#resetInterval();
    this.#expireSilence();
    if (!this.#running) return;
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
    if (previous && previous.activity.stage !== segment.activity.stage)
      // A live change during the stage end may have replaced or removed the next segment.
      return void this.#endStage(previous.activity, () => {
        const next = segmentAt(this.#segments, this.#elapsed);
        if (next) this.#open(next, previous);
        else this.#complete();
      });
    this.#open(segment, previous);
  }

  #open(segment: Segment, previous: Segment | null): void {
    this.#active = segment;
    const show = () =>
      this.#transition(segment.phase, segment.activity.stage, segment.start);
    this.#cancelEarly();
    this.#stabilityAt = -Infinity;
    this.#continuity++;
    if (previous?.activity.stage === segment.activity.stage) {
      show();
      return this.#measureStage();
    }
    const generation = ++this.#generation;
    this.#clock.hold();
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
        this.#tickAt = this.#clock.resume();
        if (!previous) show();
        if (segment.phase !== "warmup") this.#measureStage();
        this.#tick();
        this.#arm();
      },
      (cause) => {
        if (generation !== this.#generation) return;
        this.#fail(
          classify(cause, "protocol-error"),
          cause instanceof Error ? cause.message : "Stage preparation failed",
        );
      },
    );
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
      this.#participants(),
      activity,
    );
    for (const server of this.#participants()) {
      server.down = 0;
      server.up = null;
      server.latency.failed.delete(activity.stage);
    }
    const seed = `r${Math.round(this.#clock.start)}`;
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
      const now = this.#clock.active();
      server.progressAt = { down: now, up: now };
      server.stage?.measure();
    }
    if (!isTransfer(activity.stage)) return;
    this.#live.reset(
      Object.fromEntries(this.#ids().map((id) => [id, 0])),
      this.#clock.read(),
    );
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
    const sample = this.#aggregate.observe(boundary, final);
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
    const elapsed = this.#position();
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
    this.#ending = true;
    this.#clock.hold();
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
    // A latency channel not back by its stage end fails there; a throughput stall below the silence limit does not.
    for (const server of this.#participants())
      if (server.latencyStall)
        this.#failLatency(server, activity.stage, server.latencyStall.detail);
    for (const server of this.#stageParticipants(activity)) end(server);
    await Promise.all(ending.values());
    if (generation !== this.#generation) return;
    this.#latencyOpen = false;
    for (const server of this.#servers) {
      this.#flushLatency(server);
      const summary = server.latency.summary(activity.stage);
      this.#emit({
        type: "serverLatencySummary",
        serverId: server.server.id,
        stage: activity.stage,
        summary,
      });
      this.#cancelRecovery(server);
      server.latencyStall = null;
      server.stage = null;
    }
    const status = this.#settle(activity.stage);
    this.#emit({ type: "stageEnd", stage: activity.stage, status });
    this.#emit({ type: "serverDetails", details: this.details() });
    this.#ending = false;
    this.#tickAt = this.#clock.resume();
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
        const checkpoint = result.status === "fulfilled" ? result.value : null;
        if (checkpoint && checkpoint.bytes > (server.up?.bytes ?? -1))
          server.progressAt.up = this.#clock.active();
        boundary.up[server.server.id] = checkpoint;
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
        server.progressAt.down = run.#clock.active();
        run.#aggregate.addDownload(stage, id, bytes);
      },
      receiver(checkpoint) {
        if (!run.#measuring || !live()) return;
        if (checkpoint.bytes > (server.up?.bytes ?? -1))
          server.progressAt.up = run.#clock.active();
        server.up = checkpoint;
        if (!run.#stalled)
          run.#live.receiver(id, checkpoint, run.#clock.read());
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
        if (server.recovery)
          run.#live.restart(id, server.down, run.#clock.read());
        run.#cancelRecovery(server);
        server.latencyStall = null;
        run.#updateStalled();
      },
      stallLatency(detail) {
        server.gaps++;
        run.#stallLatency(server, detail);
      },
      resumeLatency() {
        server.latencyStall = null;
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
          run.#live.hint(id, lane, bytes, elapsedMs, run.#clock.read());
      },
    };
  }

  #position(): number {
    return (
      this.#elapsed +
      (this.#clock.held ? 0 : Math.max(0, this.#clock.read() - this.#tickAt))
    );
  }

  /** Translates a window-clock observation using the last timeline tick, not its delivery time. */
  #observationTime(observedAtMs: number): number {
    return Math.max(
      this.#active?.start ?? 0,
      Math.min(this.#position(), this.#elapsed + observedAtMs - this.#tickAt),
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
    const now = this.#clock.read();
    if (now - server.summaryAt >= SUMMARY_CADENCE_MS) {
      server.summaryAt = now;
      const summary = server.latency.summary(stage);
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
    if (server.removed || !activity) return;
    if (activity.stage === "latency")
      return this.#stallLatency(server, info.detail ?? "Latency interrupted");
    if (!this.#measuring) return;
    if (!server.recovery) {
      server.recovery = { abort: new AbortController(), info };
      this.#live.restart(server.server.id, server.down, this.#clock.read());
      this.#cancelEarly();
      this.#resetStability();
      this.#updateStalled();
    }
    // An unknown upload id grants one replacement receiver per server and run, even mid-recovery.
    if (info.rotate && info.direction === "up" && !server.rotated) {
      server.rotated = true;
      void server.stage?.replaceUpload?.(server.recovery.abort.signal);
    }
  }

  #stallLatency(server: Participant, detail: string): void {
    const stage = this.#activity?.stage;
    if (
      server.removed ||
      !stage ||
      server.latency.failed.has(stage) ||
      server.latencyStall
    )
      return;
    server.latencyStall = { at: this.#clock.active(), detail };
    this.#updateStalled();
  }

  #failLatency(
    server: Participant,
    stage: TransportRole,
    detail: string,
  ): void {
    if (stage === "latency")
      return this.#remove(server, "connection-lost", detail);
    server.latencyStall = null;
    server.latency.failed.add(stage);
    server.latency.stages[stage].markIncomplete();
    this.#failure(server, "latency", "connection-lost", detail);
  }

  /** Silence counts only active run time, so a page timer gap is never a server's stall. */
  #expireSilence(): void {
    const activity = this.#activity;
    if (!activity) return;
    const active = this.#clock.active();
    for (const server of this.#participants()) {
      const stall = server.latencyStall;
      if (stall && active - stall.at >= LATENCY_RECOVERY_BUDGET_MS)
        this.#failLatency(server, activity.stage, stall.detail);
      if (!this.#measuring) continue;
      const silent = activity.transfer.find(
        (dir) =>
          active - server.progressAt[dir] >= DIRECTION_PROGRESS_WINDOW_MS,
      );
      if (silent)
        this.#remove(
          server,
          server.recovery?.info.reason ?? "timeout",
          server.recovery?.info.detail ?? `${silent} direction carried no data`,
        );
    }
    this.#updateStalled();
  }

  /** A reported stall counts once its direction's evidence is quiet on the run clock too. */
  #recovering(server: Participant): boolean {
    const activity = this.#activity;
    if (activity?.stage === "latency") return !!server.latencyStall;
    const dir = server.recovery?.info.direction ?? activity?.transfer[0];
    return (
      !!server.recovery &&
      !!dir &&
      this.#clock.active() - server.progressAt[dir] >= STALL_QUIET_MS
    );
  }

  #cancelRecovery(server: Participant): void {
    server.recovery?.abort.abort();
    server.recovery = null;
  }

  /** The run is stalled while every participant is recovering its measured evidence. */
  #updateStalled(): void {
    const latencyStage = this.#activity?.stage === "latency";
    const servers = latencyStage
      ? this.#latencyParticipants()
      : this.#participants();
    const stalled =
      servers.length > 0 && servers.every((server) => this.#recovering(server));
    if (stalled === this.#stalled || !this.#running) return;
    this.#stalled = stalled;
    this.#breakContinuity();
    if (!stalled) {
      this.#live.reset(this.#counts(), this.#clock.read());
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
    // Server-supplied detail is bounded and filtered before it can reach saved history.
    const failure: ServerFailure = {
      serverId,
      stage,
      atMs: this.#now(),
      scope,
      reason,
      message: safeDetail(message, 256),
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
      server.latencyStall = null;
      // An unreachable server would hold each later stage's preparation until it timed out.
      if (
        (reason === "connection-lost" || reason === "timeout") &&
        this.#ids().length > 1
      ) {
        server.removed = true;
        server.stage?.discard(true);
        server.stage = null;
      }
      this.#failure(server, "latency", reason, message);
      this.#updateStalled();
      if (!this.#latencyParticipants().length) this.#skipStage();
      return;
    }
    server.removed = true;
    this.#cancelRecovery(server);
    server.latencyStall = null;
    // Failed-stage shutdown accounts for buffered probes before the worker is terminated.
    server.stage?.discard(true);
    server.stage = null;
    this.#live.drop(server.server.id);
    this.#failure(server, "throughput", reason, message);
    const survivors = this.#ids();
    this.#updateStalled();
    // A sole server skips to its next stage; several that all fail end the run as incomplete.
    if (!survivors.length && this.#hasMeasured)
      return this.#servers.length === 1
        ? this.#skipStage()
        : this.end(
            reason,
            "No server was left to run this stage",
            this.#servers,
          );
    if (!survivors.length)
      return this.#fail(
        reason,
        `All selected servers failed. ${server.server.name}: ${message}`,
      );
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
    this.#live.reset(this.#counts(), this.#clock.read());
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
    if (segment.phase !== "latency")
      this.#aggregate.trackStable(confidence.score);
    this.#stabilityAt = this.#clock.read();
    return this.#updateEarly(segment, segment.phase, confidence);
  }

  #canComplete(phase: TransportRole): boolean {
    const latency = phase === "latency";
    const servers = latency
      ? this.#latencyParticipants()
      : this.#participants();
    return (
      (latency ? servers.length > 0 : this.#aggregate.sufficient) &&
      !servers.some((server) => this.#recovering(server))
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

  #failed(stage: TransportRole, serverId?: string): boolean {
    const scope = stage === "latency" ? "latency" : "throughput";
    return this.#failures.some(
      (failure) =>
        failure.stage === stage &&
        failure.scope === scope &&
        (!serverId || failure.serverId === serverId),
    );
  }

  #reduce(stage: TransportRole): void {
    const results = this.#results;
    if (stage === "latency") {
      results.latency = this.#latencySource.latency.result();
      if (results.latency)
        this.#emit({ type: "stageResult", stage, result: results.latency });
      return;
    }
    const lanes = this.#aggregate.result(
      stage,
      this.#completedEarly.has(stage),
    );
    const path = (id: string) => {
      const server = this.#servers.find((entry) => entry.server.id === id);
      return server ? pathEvidence(server.paths).throughput : null;
    };
    const [down, up] = (["down", "up"] as const).map((dir) =>
      lanes[dir]
        ? headlineWire(this.#aggregate.headline(stage, dir), dir, path)
        : null,
    );
    if (stage === "bidirectional") {
      results.bidirectional = {
        ...lanes,
        wire: down && up && wireModel(combineCompensationEstimates([down, up])),
      };
      return;
    }
    const estimate = stage === "download" ? down : up;
    const lane = lanes[stage === "download" ? "down" : "up"];
    const result = lane && { ...lane, wire: estimate && wireModel(estimate) };
    results[stage] = result;
    if (result) this.#emit({ type: "stageResult", stage, result });
  }

  /** A stage's results and status are reduced and emitted once, the moment it ends. */
  #settle(stage: TransportRole): StageStatus {
    const settled = this.#settled[stage];
    if (settled) return settled;
    const entered = this.#entered.has(stage);
    if (!entered && !planned(this.#cfg!, stage))
      return (this.#settled[stage] = "not-run");
    if (entered) this.#reduce(stage);
    const lanes = stageLanes(this.#results, stage);
    const failed = this.#failed(stage);
    if (entered && !failed && !lanes.every(Boolean)) {
      const ids =
        stage === "latency"
          ? [this.#latencySource.server.id]
          : (this.#aggregate.intervals.findLast(
              (interval) => interval.stage === stage,
            )?.participants ?? this.#ids());
      const scope = stage === "latency" ? "latency" : "throughput";
      for (const id of ids)
        this.#record(
          id,
          scope,
          "insufficient-evidence",
          "Too little measured evidence for a result",
          stage,
        );
    }
    return (this.#settled[stage] = stageStatus(lanes, failed));
  }

  #complete(): void {
    this.#running = false;
    this.#completed = true;
    const durationMs = this.#now();
    const source = this.#latencySource.latency;
    const stages = Object.fromEntries(
      STAGES.map((stage) => [stage, this.#settle(stage)]),
    ) as RunResult["stages"];
    const statuses = Object.values(stages);
    const result: RunResult = {
      ...this.#results,
      stages,
      addedLatency: source.addedLatency(),
      latencyByStage: source.summaries(),
      multiServer: this.details(),
      outcome: statuses.includes("failed")
        ? "incomplete"
        : this.#failures.length
          ? "partial"
          : "complete",
      startedAt: this.#clock.startedAt,
      durationMs,
    };
    this.#release();
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
    const aggregate = this.#aggregate;
    return {
      selection: [...this.#servers, ...this.#dropped].map(
        ({ server }) => server,
      ),
      participants: this.#ids(),
      latencyFocus: this.#latencySource.server.id,
      intervals: structuredClone(aggregate.intervals),
      omittedIntervals: aggregate.omittedIntervals,
      failures: [...this.#failures],
      servers: this.#servers.map(({ server, paths, latency }) => {
        const result = (stage: TransferStage, dir: FlowDirection) =>
          aggregate.serverResult(stage, dir, server.id);
        const evidence = {
          latency: latency.result(),
          download: result("download", "down"),
          upload: result("upload", "up"),
          bidirectional: this.#entered.has("bidirectional")
            ? {
                down: result("bidirectional", "down"),
                up: result("bidirectional", "up"),
              }
            : null,
        };
        const status = (stage: TransportRole): StageStatus =>
          (this.#settled[stage] ?? "not-run") === "not-run" ||
          (stage === "latency" && !paths.latency)
            ? "not-run"
            : stageStatus(
                stageLanes(evidence, stage),
                this.#failed(stage, server.id),
              );
        return {
          server,
          ...pathEvidence(paths),
          ...evidence,
          latencyByStage: latency.summaries(),
          addedLatency: latency.addedLatency(),
          totalBytes: aggregate.totals(server.id),
          stages: Object.fromEntries(
            STAGES.map((stage) => [stage, status(stage)]),
          ) as Record<TransportRole, StageStatus>,
        };
      }),
    };
  }
}
