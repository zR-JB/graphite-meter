import type {
  CoreHost,
  RunnerBackend,
  RunMeasurementSource,
} from "../runner/core";
import { RunnerCore, STAGE_RECOVERY_BUDGET_MS } from "../runner/core";
import { RealBackend } from "../runner/RealRunner";
import { RunAccumulator } from "../runner/evaluation";
import { LatencyPresentationBuckets } from "../runner/latencyBuckets";
import { fixedPingIntervalMs } from "../runner/pingCadence";
import { GrowingRateEstimator } from "../runner/rateEstimator";
import type {
  FlowDirection,
  LatencyObservation,
  LiveRunConfig,
  NetworkRunner,
  PhaseActivity,
  PreparedPaths,
  ReceiverCheckpoint,
  RunnerConfig,
  RunnerEvent,
  RunResult,
  StallInfo,
  TransportRole,
} from "../runner/contract";
import { identity, type ServerIdentity } from "./catalog";
import {
  AggregateMeasurements,
  weakestLatencyConfidence,
  type Boundary,
  type MultiServerResult,
  type ServerFailure,
  type TransferStage,
} from "./measurement";
import { planServerStreams, type ServerStreamPlan } from "./streamBudget";

export interface PreparedServer {
  server: ServerIdentity;
  paths: PreparedPaths;
}
export interface ParticipantTransport extends RunnerBackend {
  waitForReadiness?(signal: AbortSignal): Promise<void>;
  flushDownload(now: number): void;
  checkpoint(signal: AbortSignal): Promise<ReceiverCheckpoint | null>;
}
interface Participant extends PreparedServer {
  backend: ParticipantTransport;
  accum: RunAccumulator;
  buckets: LatencyPresentationBuckets;
  rates: Record<FlowDirection, GrowingRateEstimator>;
  removed: boolean;
  latencyFailed: boolean;
  down: number;
  recovery: {
    abort: AbortController;
    timer: ReturnType<typeof setTimeout>;
  } | null;
  latencyTimer: ReturnType<typeof setTimeout> | null;
  summaryAt: number;
}
const CHECKPOINT_CADENCE_MS = 250;

/** One schedule drives all server-owned resources. Membership and receiver windows belong to this coordinator. */
export class ServerCoordinator implements NetworkRunner, RunMeasurementSource {
  readonly #core: RunnerCore;
  readonly #servers: Participant[];
  readonly #aggregate = new AggregateMeasurements();
  #handlers = new Set<(event: RunnerEvent) => void>();
  #config: RunnerConfig | null = null;
  #activity: PhaseActivity | null = null;
  #streamPlan: ServerStreamPlan = {};
  #measuring = false;
  #latencyMeasuring = false;
  #runStart = 0;
  #focus: string;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #boundaryAbort = new AbortController();
  #boundary: Promise<void> | null = null;
  #epoch = 0;
  #failures: ServerFailure[] = [];
  #ledgerReported: Record<FlowDirection, number> = { down: 0, up: 0 };
  #completed = false;
  #hasMeasured = false;

  constructor(
    servers: PreparedServer[],
    focus: string,
    createBackend: (
      paths: PreparedPaths,
      count: (activity: PhaseActivity, dir: FlowDirection) => number,
    ) => ParticipantTransport = (paths, count) => new RealBackend(paths, count),
  ) {
    if (
      servers.length < 1 ||
      servers.length > 4 ||
      new Set(servers.map((server) => server.server.id)).size !== servers.length
    )
      throw new Error("Select one to four different servers");
    this.#focus = servers.some((server) => server.server.id === focus)
      ? focus
      : servers[0].server.id;
    this.#servers = servers.map((server) => ({
      ...server,
      server: identity(server.server),
      backend: createBackend(
        server.paths,
        (_activity, dir) => this.#streamPlan[server.server.id]?.[dir] ?? 1,
      ),
      accum: new RunAccumulator(),
      buckets: new LatencyPresentationBuckets(),
      rates: {
        down: new GrowingRateEstimator(),
        up: new GrowingRateEstimator(),
      },
      removed: false,
      latencyFailed: false,
      down: 0,
      recovery: null,
      latencyTimer: null,
      summaryAt: 0,
    }));
    const backend: RunnerBackend = {
      attach: () => {},
      onRunStart: (config) => this.#beginRun(config),
      onStageBegin: (activity) => this.#beginStage(activity),
      onStageMeasure: (activity) => this.#measureStage(activity),
      onStageEnd: (activity, flush) => this.#endStage(activity, flush),
      onAbort: () => this.#release(),
      onComplete: () => this.#release(),
    };
    this.#core = new RunnerCore(backend, this);
    for (const server of this.#servers)
      server.backend.attach(this.#host(server));
    this.#core.on((event) => {
      // Server-tagged observations below carry every latency population to presentation.
      if (event.type === "latency" || event.type === "latencySummary") return;
      if (event.type === "complete") {
        this.#completed = true;
        event.result.outcome = this.#failures.length ? "partial" : "complete";
        event.result.latency = this.#config
          ? this.latencyResult(this.#config)
          : null;
      }
      this.#emit(event);
    });
  }
  get phase() {
    return this.#core.phase;
  }
  start(config: RunnerConfig, preTestPingMs: number): void {
    // Reject resource conflicts before opening any measured connections.
    const activities: PhaseActivity[] = [
      {
        stage: "download",
        transfer: ["down"],
        loadedLatency:
          !config.skipLoadedLatencyWhenStageOff || config.stages.latency,
      },
      {
        stage: "upload",
        transfer: ["up"],
        loadedLatency:
          !config.skipLoadedLatencyWhenStageOff || config.stages.latency,
      },
      {
        stage: "bidirectional",
        transfer: ["down", "up"],
        loadedLatency:
          !config.skipLoadedLatencyWhenStageOff || config.stages.latency,
      },
    ];
    for (const activity of activities)
      if (config.stages[activity.stage])
        planServerStreams(
          config,
          this.#servers.map((server) => ({
            id: server.server.id,
            paths: server.paths,
          })),
          activity,
        );
    this.#core.start(config, preTestPingMs);
  }
  abort(): void {
    this.#core.abort();
  }
  dispose(): void {
    this.#core.dispose();
    this.#release();
    this.#handlers.clear();
  }
  on(handler: (event: RunnerEvent) => void): () => void {
    this.#handlers.add(handler);
    return () => this.#handlers.delete(handler);
  }
  reconfigure(config: LiveRunConfig): void {
    if (this.#config) this.#config = { ...this.#config, ...config };
    this.#core.reconfigure(config);
  }
  focusServer(id: string): void {
    if (this.#servers.some((server) => server.server.id === id)) {
      this.#focus = id;
      this.#emit({ type: "serverDetails", details: this.details() });
    }
  }
  #emit(event: RunnerEvent): void {
    for (const handler of this.#handlers) handler(event);
  }
  #now(): number {
    return Math.max(0, performance.now() - this.#runStart);
  }
  #active(): Participant[] {
    return this.#servers.filter((server) => !server.removed);
  }
  #latencyServers(): Participant[] {
    return this.#active().filter((server) => server.paths.latency !== null);
  }
  #stageParticipants(activity: PhaseActivity): Participant[] {
    return activity.stage === "latency"
      ? this.#latencyServers()
      : this.#active();
  }
  #focused(): Participant {
    return this.#servers.find((server) => server.server.id === this.#focus)!;
  }

  #beginRun(config: RunnerConfig): void {
    this.#release();
    this.#completed = false;
    this.#hasMeasured = false;
    this.#config = config;
    this.#runStart = performance.now();
    this.#failures = [];
    this.#aggregate.reset();
    this.#ledgerReported = { down: 0, up: 0 };
    this.#boundaryAbort = new AbortController();
    for (const server of this.#servers) {
      server.removed = false;
      server.latencyFailed = false;
      server.down = 0;
      server.accum.reset();
      server.backend.onRunStart(config);
    }
  }
  async #beginStage(activity: PhaseActivity): Promise<void> {
    this.#activity = activity;
    this.#measuring = this.#latencyMeasuring = false;
    this.#streamPlan = planServerStreams(
      this.#config!,
      this.#active().map((server) => ({
        id: server.server.id,
        paths: server.paths,
      })),
      activity,
    );
    for (const server of this.#active()) {
      server.latencyFailed = false;
      server.accum.beginPhase();
      server.down = 0;
      server.rates.down.reset();
      server.rates.up.reset();
    }
    const epoch = ++this.#epoch;
    const participants = this.#stageParticipants(activity);
    const results = await Promise.allSettled(
      participants.map(async (server) => {
        await server.backend.onStageBegin(activity);
        await server.backend.waitForReadiness?.(this.#boundaryAbort.signal);
        if (
          activity.transfer.includes("up") &&
          !(await server.backend.checkpoint(this.#boundaryAbort.signal))
        )
          throw new Error("Fresh upload receiver evidence is unavailable");
      }),
    );
    if (epoch !== this.#epoch) return;
    for (const [index, result] of results.entries()) {
      if (result.status !== "rejected") continue;
      const server = participants[index];
      const message =
        result.reason instanceof Error
          ? result.reason.message
          : "Measurement preparation failed";
      if (!this.#hasMeasured)
        throw new Error(
          `${server.server.name}: ${message}. Resolve the selection before starting.`,
        );
      this.#remove(server, "preparation-failed", message);
    }
  }
  #measureStage(activity: PhaseActivity): void {
    this.#hasMeasured = true;
    this.#measuring = true;
    this.#latencyMeasuring = true;
    for (const server of this.#stageParticipants(activity)) {
      server.buckets.reset(
        this.#core.elapsed,
        activity.stage,
        activity.stage !== "latency",
        this.#epoch,
        this.#config!.duration[`${activity.stage}Ms`],
        fixedPingIntervalMs(
          activity.stage === "latency"
            ? this.#config!.pingCadence
            : this.#config!.loadedPingCadence,
        ),
      );
      server.backend.onStageMeasure(activity);
    }
    if (activity.stage !== "latency") {
      this.#aggregate.begin(
        activity.stage,
        this.#active().map((server) => server.server.id),
        this.#now(),
      );
      void this.#sampleBoundary();
    }
  }
  #armBoundary(): void {
    if (this.#timer) clearTimeout(this.#timer);
    if (this.#measuring && this.#activity?.transfer.length)
      this.#timer = setTimeout(() => {
        this.#timer = null;
        void this.#sampleBoundary();
      }, CHECKPOINT_CADENCE_MS);
  }
  async #sampleBoundary(final = false): Promise<void> {
    if (this.#boundary) {
      await this.#boundary;
      if (!final) return;
    }
    if (
      (!this.#measuring && !final) ||
      !this.#activity?.transfer.length ||
      this.#completed
    )
      return;
    const epoch = this.#epoch,
      stage = this.#activity.stage as TransferStage,
      participants = this.#active();
    const at = performance.now();
    for (const server of participants) server.backend.flushDownload(at);
    const boundary: Boundary = {
      atMs: at - this.#runStart,
      down: Object.fromEntries(
        participants.map((server) => [server.server.id, server.down]),
      ),
      up: {},
    };
    if (final) this.#measuring = false;
    const work = async () => {
      if (this.#activity?.transfer.includes("up")) {
        const results = await Promise.allSettled(
          participants.map((server) =>
            server.backend.checkpoint(this.#boundaryAbort.signal),
          ),
        );
        participants.forEach((server, index) => {
          const result = results[index];
          boundary.up[server.server.id] =
            result.status === "fulfilled" ? result.value : null;
        });
      }
      if (epoch !== this.#epoch || this.#boundaryAbort.signal.aborted) return;
      const previousInterval = this.#aggregate.current?.id;
      const sample = this.#aggregate.observe(boundary);
      this.#emit({ type: "aggregateEvidence", available: sample !== null });
      if (!sample || previousInterval !== this.#aggregate.current?.id)
        this.#core.resetMeasurementInterval();
      if (sample)
        for (const dir of this.#activity!.transfer) {
          const rate =
            dir === "down" ? sample.downBytesPerSec : sample.upBytesPerSec;
          if (rate === null) continue;
          const durationSec = (sample.endMs - sample.startMs) / 1000;
          const total = this.#servers.reduce(
            (bytes, server) =>
              bytes + this.#aggregate.totals(server.server.id)[dir],
            0,
          );
          const uniqueDelta = Math.max(0, total - this.#ledgerReported[dir]);
          this.#ledgerReported[dir] = total;
          this.#core.ingestThroughput(
            dir,
            rate * durationSec,
            durationSec,
            dir === "up",
            true,
            uniqueDelta,
          );
        }
      if (final) this.#aggregate.close();
      if (stage !== this.#activity?.stage) return;
      this.#emit({ type: "serverDetails", details: this.details() });
    };
    this.#boundary = work();
    try {
      await this.#boundary;
    } finally {
      this.#boundary = null;
      if (!final) this.#armBoundary();
    }
  }
  async #endStage(activity: PhaseActivity, flush = true): Promise<void> {
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    if (flush && activity.transfer.length) await this.#sampleBoundary(true);
    this.#measuring = false;
    await Promise.allSettled(
      this.#stageParticipants(activity).map((server) =>
        server.backend.onStageEnd(activity, flush),
      ),
    );
    this.#latencyMeasuring = false;
    for (const server of this.#servers) {
      const sample = server.buckets.flush(this.#core.elapsed);
      if (sample)
        this.#emit({
          type: "serverLatency",
          serverId: server.server.id,
          sample,
        });
      this.#emit({
        type: "serverLatencySummary",
        serverId: server.server.id,
        stage: activity.stage,
        summary: server.accum.latencySummary(activity.stage),
      });
      this.#cancelRecovery(server);
      this.#resumeLatency(server);
    }
    this.#emit({ type: "serverDetails", details: this.details() });
  }
  #release(): void {
    this.#epoch++;
    this.#measuring = this.#latencyMeasuring = false;
    if (this.#timer) clearTimeout(this.#timer);
    this.#timer = null;
    this.#boundaryAbort.abort();
    this.#aggregate.close();
    for (const server of this.#servers) {
      this.#cancelRecovery(server);
      this.#resumeLatency(server);
      server.backend.onAbort();
    }
  }
  #cancelRecovery(server: Participant): void {
    if (server.recovery) {
      clearTimeout(server.recovery.timer);
      server.recovery.abort.abort();
      server.recovery = null;
    }
  }
  #stall(server: Participant, info: StallInfo): void {
    if (server.removed || server.recovery || !this.#activity) return;
    if (this.#activity.stage === "latency") {
      this.#stallLatency(server, info.detail ?? "Latency interrupted");
      return;
    }
    const abort = new AbortController();
    const timer = setTimeout(() => {
      if (server.recovery?.abort === abort)
        this.#remove(
          server,
          "connection-lost",
          info.detail ?? "Server stopped delivering measured data",
        );
    }, STAGE_RECOVERY_BUDGET_MS);
    server.recovery = { abort, timer };
    this.#core.resetMeasurementInterval();
    void Promise.resolve(
      server.backend.onStageRecovery?.({
        stage: this.#activity.stage,
        direction: info.direction,
        cause: info.recoveryCause ?? "transient-connection",
        signal: abort.signal,
      }),
    ).catch(() => {});
  }
  #stallLatency(server: Participant, detail: string): void {
    if (server.removed || server.latencyFailed || server.latencyTimer) return;
    server.latencyTimer = setTimeout(() => {
      server.latencyTimer = null;
      server.latencyFailed = true;
      if (this.#activity) {
        server.accum.markLatencyAccountingIncomplete(this.#activity.stage);
        this.#failure(server, "latency", "connection-lost", detail);
      }
    }, STAGE_RECOVERY_BUDGET_MS);
  }
  #resumeLatency(server: Participant): void {
    if (server.latencyTimer) clearTimeout(server.latencyTimer);
    server.latencyTimer = null;
  }
  #failure(
    server: Participant,
    scope: ServerFailure["scope"],
    reason: string,
    message: string,
  ): void {
    if (!this.#activity) return;
    const failure: ServerFailure = {
      serverId: server.server.id,
      stage: this.#activity.stage,
      atMs: this.#now(),
      scope,
      reason,
      message,
    };
    if (
      this.#failures.some(
        (old) =>
          old.serverId === failure.serverId &&
          old.stage === failure.stage &&
          old.scope === scope,
      )
    )
      return;
    this.#failures.push(failure);
    this.#emit({
      type: "serverFailure",
      failure,
      participants: this.#active().map((server) => server.server.id),
    });
  }
  #remove(server: Participant, reason: string, message: string): void {
    if (server.removed || this.#completed) return;
    if (!this.#hasMeasured) {
      this.#core.fail(
        "connection-lost",
        `${server.server.name}: ${message}. Resolve the selection before starting.`,
      );
      return;
    }
    if (this.#activity?.stage === "latency") {
      server.latencyFailed = true;
      server.accum.markLatencyAccountingIncomplete("latency");
      this.#failure(server, "latency", reason, message);
      return;
    }
    server.removed = true;
    this.#cancelRecovery(server);
    this.#resumeLatency(server);
    server.backend.onAbort();
    this.#failure(server, "throughput", reason, message);
    this.#epoch++;
    this.#emit({ type: "aggregateEvidence", available: false });
    this.#core.resetMeasurementInterval();
    if (this.#activity)
      this.#aggregate.begin(
        this.#activity.stage,
        this.#active().map((server) => server.server.id),
        this.#now(),
        "dropout",
      );
    if (!this.#active().length) {
      this.#finishIncomplete();
      return;
    }
    void this.#sampleBoundary();
  }
  #finishIncomplete(): void {
    this.#completed = true;
    this.#core.abort();
    this.#emit({ type: "complete", result: this.#result("incomplete") });
  }
  #result(outcome: RunResult["outcome"]): RunResult {
    return {
      download: this.throughputResult("download", false),
      upload: this.throughputResult("upload", false),
      bidirectional: this.#config?.stages.bidirectional
        ? this.bidirectionalResult(false)
        : null,
      latency: this.#config ? this.latencyResult(this.#config) : null,
      latencyByStage: this.latencySummaries(),
      bufferbloat: this.bufferbloatGrade(),
      stageFailures: {},
      startedAt: Date.now() - this.#now(),
      durationMs: this.#now(),
      multiServer: this.details(),
      outcome,
    };
  }
  #host(server: Participant): CoreHost {
    const owner = this;
    return {
      get config() {
        return owner.#core.config;
      },
      get phase() {
        return owner.#core.phase;
      },
      get elapsed() {
        return owner.#core.elapsed;
      },
      ingestThroughput(dir, bytes, seconds, authoritative) {
        if (
          !owner.#measuring ||
          server.removed ||
          !owner.#activity ||
          owner.#activity.stage === "latency"
        )
          return;
        if (dir === "down") {
          server.down += Math.max(0, bytes);
          owner.#aggregate.addDownload(server.server.id, bytes);
        }
        server.accum.pushThroughput(
          owner.#activity.stage,
          dir,
          bytes,
          seconds,
          authoritative,
          owner.#now(),
        );
        server.rates[dir].observe({ bytes, durationMs: seconds * 1000 });
      },
      ingestReceiver(checkpoint) {
        if (owner.#measuring && !server.removed)
          owner.#aggregate.observeUpload(server.server.id, checkpoint);
      },
      ingestLatency(observation) {
        owner.#latency(server, observation);
      },
      ingestLatencyInterruption(count, reason) {
        if (owner.#latencyMeasuring && owner.#activity)
          server.accum.interruptLatency(owner.#activity.stage, count, reason);
      },
      ingestLatencyAccountingIncomplete() {
        if (owner.#activity) {
          server.accum.markLatencyAccountingIncomplete(owner.#activity.stage);
          owner.#failure(
            server,
            "latency",
            "connection-lost",
            "Latency observations were interrupted",
          );
        }
      },
      recordRecoveryGap(dir, seconds) {
        if (owner.#activity?.stage && owner.#activity.stage !== "latency")
          server.accum.recordRecoveryGap(owner.#activity.stage, dir, seconds);
      },
      recordRecoveryBytes(dir, bytes) {
        if (owner.#activity?.stage && owner.#activity.stage !== "latency")
          server.accum.recordRecoveryBytes(owner.#activity.stage, dir, bytes);
      },
      stall(info) {
        owner.#stall(server, info);
      },
      resume() {
        owner.#cancelRecovery(server);
        owner.#resumeLatency(server);
      },
      stallLatency(detail) {
        owner.#stallLatency(server, detail);
      },
      resumeLatency() {
        owner.#resumeLatency(server);
      },
      emit(event) {
        if (event.type !== "uploadPresentation") owner.#emit(event);
      },
      fail(reason, message) {
        owner.#remove(server, reason, message);
      },
      failStage(_stage, reason, message) {
        owner.#remove(server, reason, message);
      },
      presentationRate(dir) {
        return server.rates[dir].snapshot().presentedBytesPerSec;
      },
    };
  }
  #latency(server: Participant, observation: LatencyObservation): void {
    if (!this.#latencyMeasuring || server.removed || !this.#activity) return;
    const stage = this.#activity.stage;
    const t = this.#core.observationTime(observation.observedAtMs);
    server.accum.pushLatency(
      stage,
      observation.rttMs,
      observation.lost,
      t,
      this.#epoch,
      observation.rttEligible,
      observation.reflectorHandlingMs,
    );
    if (observation.rttEligible !== false)
      for (const sample of server.buckets.observe(
        t,
        observation.rttMs,
        observation.lost,
      ))
        this.#emit({
          type: "serverLatency",
          serverId: server.server.id,
          sample,
        });
    if (performance.now() - server.summaryAt >= 1000) {
      server.summaryAt = performance.now();
      this.#emit({
        type: "serverLatencySummary",
        serverId: server.server.id,
        stage,
        summary: server.accum.latencySummary(stage),
      });
    }
    // The core uses the source's server populations for all reduction and confidence.
    // Feeding every observation keeps the one schedule responsive if any latency path fails.
    this.#core.ingestLatency(observation);
  }
  confidence(stage: TransportRole) {
    return stage === "latency"
      ? weakestLatencyConfidence(
          this.#latencyServers()
            .filter((server) => !server.latencyFailed)
            .map((server) => server.accum),
        )
      : this.#aggregate.confidence();
  }
  trackStableRun(
    stage: TransportRole,
    score: number,
    cfg: RunnerConfig["adaptive"],
  ): boolean {
    if (stage !== "latency") return this.#aggregate.trackStable(score, cfg);
    return this.#latencyServers()
      .filter((server) => !server.latencyFailed)
      .map((server) => server.accum.trackStableRun(stage, score, cfg))
      .every(Boolean);
  }
  canComplete(stage: TransportRole): boolean {
    if (stage === "latency")
      return (
        this.#latencyServers().some((server) => !server.latencyFailed) &&
        this.#latencyServers().every(
          (server) => server.latencyFailed || !server.latencyTimer,
        )
      );
    return (
      !!this.#aggregate.current?.complete &&
      this.#active().every((server) => !server.recovery)
    );
  }
  throughputResult(stage: "download" | "upload", stable: boolean) {
    return this.#aggregate.result(
      stage,
      stage === "download" ? "down" : "up",
      stable,
    );
  }
  bidirectionalResult(stable: boolean) {
    return {
      down: this.#aggregate.result("bidirectional", "down", stable),
      up: this.#aggregate.result("bidirectional", "up", stable),
    };
  }
  latencyResult(config: RunnerConfig) {
    return this.#focused().accum.latencyResult(config);
  }
  latencySummaries() {
    return this.#focused().accum.latencySummaries();
  }
  bufferbloatGrade() {
    return this.#focused().accum.bufferbloatGrade();
  }
  details(): MultiServerResult {
    const config = this.#config;
    return {
      selection: this.#servers.map((server) => server.server),
      participants: this.#active().map((server) => server.server.id),
      latencyFocus: this.#focus,
      intervals: structuredClone(this.#aggregate.intervals),
      omittedIntervals: this.#aggregate.omittedIntervals,
      failures: [...this.#failures],
      servers: this.#servers.map((server) => ({
        server: server.server,
        throughput: {
          origin: server.paths.throughput.target.origin,
          transport: server.paths.throughput.target.transport,
          protocol: server.paths.throughput.fetch.protocol,
          ...(server.paths.throughput.browserProtocol
            ? { browserProtocol: server.paths.throughput.browserProtocol }
            : {}),
          clientIpVersion: server.paths.throughput.probe.clientIpVersion,
        },
        latencyTarget: server.paths.latency
          ? {
              origin: server.paths.latency.target.origin,
              transport: server.paths.latency.target.transport,
            }
          : null,
        latency: config ? server.accum.latencyResult(config) : null,
        latencyByStage: server.accum.latencySummaries(),
        bufferbloat: server.accum.bufferbloatGrade(),
        download: server.accum.partialThroughputResult("download"),
        upload: server.accum.partialThroughputResult("upload"),
        bidirectional: config?.stages.bidirectional
          ? server.accum.partialBidirectionalResult()
          : null,
        totalBytes: this.#aggregate.totals(server.server.id),
      })),
    };
  }
}
