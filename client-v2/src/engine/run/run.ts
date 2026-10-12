// One run: a stage schedule over one to four servers (docs/MEASUREMENTS.md). It opens each server's resources stage
// by stage, folds their evidence into the throughput aggregate and the latency populations, judges silence, stalls
// and dropouts, finishes stable stages early, reshapes the timeline on live changes, and reduces it all to a result.
import { safeDetail } from "../../protocol/documents";
import type { FailureReason } from "../../protocol/reasons";
import { Aggregate, type Boundary, type Checkpoint, type ComponentWindow, type Direction } from "../measure/aggregate";
import { LatencyBuckets, type LatencyBucket } from "../measure/buckets";
import { ServerLatency, type LatencySummary } from "../measure/latency";
import { LiveRates } from "../measure/live";
import { combinedModel, type WireModel, type WirePath } from "../measure/overhead";
import { EARLY_FINISH, mayFinishEarly, minCoverage, type Confidence } from "../measure/stability";
import {
  CADENCE_MS,
  ServerBusyError,
  SignInRequired,
  StageFailure,
  type OpenStage,
  type PreparedServer,
  type ServerIdentity,
  type Stall,
  type StageEvents,
  type StageLanes,
  type StageResources,
  type StageSetup,
  type TestConfig,
} from "../types";
import { pageClock, RunClock, type Clock } from "./clock";
import {
  coveredMs,
  failureScope,
  lanes,
  outcomeOf,
  stageStatus,
  type MultiServerResult,
  type RunResult,
  type Scope,
  type ServerFailure,
  type ServerMeasurementSummary,
  type StagePlans,
  type StageResults,
  type StageStatus,
} from "./result";
import {
  activities,
  changeable,
  planned,
  replan,
  segmentAt,
  STAGES,
  timeline,
  truncate,
  type Activity,
  type Segment,
  type Stage,
  type TransferStage,
} from "./schedule";

const TICK_MS = 60;
const STABILITY_CADENCE_MS = 100;
const PROGRESS_CADENCE_MS = 250;
const SUMMARY_CADENCE_MS = 1_000;
/** A direction that moved nothing for this long is quiet; a server that moved within it is moving. */
const QUIET_MS = 500;
/** A direction silent this long leaves, if another server moves it meanwhile. */
const SILENCE_MS = 1_500;
/** A receiver that sends no record at all is given longer, as its feed may lag. */
const RECEIVER_SILENCE_MS = 4_000;
/** Two failed reconnects (3 s establishment and 0.5 s margin each) and their 300 ms backoff. */
const LATENCY_RECOVERY_MS = 7_300;

/** The test settings a run follows, with the warmup already adapted to the measured RTT. */
export type RunPlan = Pick<
  TestConfig,
  "stages" | "duration" | "skipLoadedLatencyWhenStageOff" | "pingCadence" | "loadedPingCadence" | "adaptive"
>;
/** What may change while a run is live. */
export type LiveChange = Pick<RunPlan, "stages" | "duration" | "adaptive">;

export interface DroppedServer {
  server: ServerIdentity;
  reason: FailureReason;
  message: string;
}

export interface RunOptions {
  /** One to four servers, in catalogue order. */
  servers: PreparedServer[];
  /** The server whose idle latency the run reports; the first when it is not among them. */
  focus: string;
  /** Selected servers that failed their start check. */
  dropped: DroppedServer[];
  plan: RunPlan;
  open: OpenStage;
  /** Lanes per server for a stage; throws when servers sharing an origin cannot all get theirs. */
  streams(servers: readonly PreparedServer[], activity: Activity): Map<string, StageLanes>;
  clock?: Clock;
  online?: () => boolean;
}

export type Phase = "connecting" | "warmup" | Stage;

/** A transfer stage's presented rates at one sampling tick; results never read them. */
export interface LiveSample {
  /** Timeline ms. */
  t: number;
  stage: TransferStage;
  /** Samples with different ids are separate lines. */
  continuityId: number;
  /** Unique measured bytes of the whole run so far. */
  bytes: number;
  /** Null when the stage does not move the direction, or before its first bytes; 0 while stalled. */
  down: number | null;
  up: number | null;
  /** The upload rate bridged by lane completions while a receiver pauses; null when nothing needed bridging. */
  bridgedUp: number | null;
  stalled: boolean;
  /** Active ms since any server moved, once stalled for at least half a second. */
  quietMs: number | null;
}

export type RunEvent =
  /** The shown segment and what its stage is doing: the first stage prepares as `connecting`; a later one shows at
      once. `t` is where the segment starts on the timeline. */
  | { type: "phase"; phase: Phase; stage: Stage; t: number; step: "preparing" | "running" | "finalizing" }
  /** The shown segment's position, at least every 250 ms while it runs, and once more as its stage finalizes. */
  | { type: "progress"; elapsedMs: number; budgetMs: number }
  | { type: "live"; sample: LiveSample }
  | { type: "latency"; serverId: string; bucket: LatencyBucket }
  | { type: "summary"; serverId: string; stage: Stage; summary: LatencySummary | null }
  | { type: "failure"; failure: ServerFailure }
  /** The run as a whole stalled (every server recovering or quiet), or recovered. */
  | { type: "stall"; stall: Stall | null }
  | { type: "stageEnd"; stage: Stage; status: StageStatus; results: StageResults }
  | { type: "complete"; result: RunResult; plans: StagePlans }
  | { type: "error"; reason: FailureReason; message: string }
  | { type: "aborted"; t: number };

/** A server across the whole run. */
interface Participant {
  id: string;
  prepared: PreparedServer;
  latency: ServerLatency;
  buckets: LatencyBuckets;
  /** Out of the run; a sole server is reinstated at each new stage. */
  removed: boolean;
  /** Whether its one replacement upload id is spent. */
  rotated: boolean;
  /** Probe adjacency for jitter; a reconnect breaks it. */
  continuity: number;
  /** Its latency bus across the run's stages, disposed when the run ends. */
  bus: StageSetup["bus"];
}

/** A server in one stage. Times are active ms. */
interface Seat {
  server: Participant;
  resources: StageResources;
  /** Measured client-consumed bytes this stage. */
  down: number;
  /** The latest measured receiver checkpoint this stage. */
  up: Checkpoint | null;
  progressAt: Record<Direction, number>;
  /** The last receiver record with advancing receiver time, and the receiver time its bytes last grew. */
  heardAt: number;
  grewNanos: number;
  recovery: { abort: AbortController; stall: Stall } | null;
  latencyStall: { at: number; detail: string } | null;
  /** Page ms of the last live summary. */
  summaryAt: number;
}

/** One entered stage. Every callback its resources make checks that it is still the run's stage and not settled. */
interface StageRun {
  activity: Activity;
  phase: "preparing" | "warmup" | "measuring" | "finalizing" | "settled";
  /** Readiness waits and checkpoints end with it. */
  abort: AbortController;
  seats: Map<string, Seat>;
  streams: Map<string, StageLanes>;
  /** The measured window began. */
  measured: boolean;
  /** Active ms when finalization began; failures after it are judged against it. */
  closedAt: number;
  /** A hole, stall, dropout or gap: no early finish this stage. */
  disturbed: boolean;
  /** Timeline ms since when an early finish has been eligible. */
  armedAt: number | null;
  /** Page ms of the last early-finish evaluation. */
  evaluatedAt: number;
  finishedEarly: boolean;
  /** The plan as the window closed; later changes cannot reach a window that already ended. */
  plan: StagePlans[Stage] | null;
}

const active = (stage: StageRun) => [...stage.seats.values()].filter((seat) => !seat.server.removed);

const quiet = (stage: StageRun, seat: Seat, at: number) =>
  stage.phase === "measuring" &&
  stage.activity.transfer.length > 0 &&
  stage.activity.transfer.every((dir) => at - seat.progressAt[dir] >= QUIET_MS);

/** The first direction silent past its limit. Uploads count on the receiver's own clock. */
const silence = (stage: StageRun, seat: Seat, at: number): Direction | undefined =>
  stage.activity.transfer.find((dir) =>
    dir === "down"
      ? at - seat.progressAt.down >= SILENCE_MS
      : ((seat.up?.nanos ?? 0) - seat.grewNanos) / 1e6 >= SILENCE_MS || at - seat.heardAt >= RECEIVER_SILENCE_MS,
  );

/** A reported stall counts once its direction's evidence is quiet on the run clock too. */
function recovering(stage: StageRun, seat: Seat, at: number): boolean {
  if (stage.activity.stage === "latency") return !!seat.latencyStall;
  const dir = seat.recovery?.stall.direction ?? stage.activity.transfer[0]!;
  return !!seat.recovery && at - seat.progressAt[dir] >= QUIET_MS;
}

const HTTP: Record<string, WirePath["protocol"]> = { "http/1.1": "http1", h2: "http2", h3: "http3" };
const NETWORK_FAILURE =
  /failed to fetch|fetch failed|network(?:error| request failed)|load failed|connection (?:refused|reset|lost)/i;

function* causes(error: unknown): Generator<Error> {
  const seen = new Set<unknown>();
  for (; error instanceof Error && !seen.has(error); error = error.cause) {
    seen.add(error);
    yield error;
  }
}

/** A busy server wins over a nested timeout; offline or a network failure is a lost connection. */
function classify(cause: unknown, fallback: FailureReason, online: boolean): FailureReason {
  const chain = [...causes(cause)];
  const known = chain.find((error) => error instanceof StageFailure);
  if (known) return known.reason;
  if (chain.some((error) => error instanceof ServerBusyError)) return "server-busy";
  if (!online || chain.some((error) => error.name === "NetworkError" || NETWORK_FAILURE.test(error.message)))
    return "connection-lost";
  if (chain.some((error) => error instanceof DOMException && error.name === "TimeoutError")) return "timeout";
  return fallback;
}

export class Run {
  readonly #servers: Participant[];
  readonly #source: Participant;
  readonly #dropped: DroppedServer[];
  readonly #open: OpenStage;
  readonly #streams: RunOptions["streams"];
  readonly #online: () => boolean;
  readonly #time: Clock;
  #clock!: RunClock;
  #handlers = new Set<(event: RunEvent) => void>();
  #state: "created" | "running" | "completed" | "aborted" | "failed" = "created";
  #plan: RunPlan;
  #segments: Segment[];
  /** The segment the run shows; a stage's warmup and window share one activity. */
  #shown: Segment | null = null;
  /** Timeline ms at the last tick, and the page ms of that tick. */
  #elapsed = 0;
  #tickAt = 0;
  #timer: (() => void) | null = null;
  /** The stage being prepared, measured or finalized; the last settled one between stages. */
  #stage: StageRun | null = null;
  #stages = new Map<Stage, StageRun>();
  #settled = new Map<Stage, { status: StageStatus; plan: StagePlans[Stage] }>();
  #results: StageResults = { latency: null, download: null, upload: null, bidirectional: null };
  /** Complete once the current stage settles. */
  #finishing = false;
  #aggregate = new Aggregate();
  #failures: ServerFailure[] = [];
  #live = new LiveRates();
  #stalled = false;
  #continuity = 0;
  #sampledAt = -Infinity;
  #progress = { key: "", at: -Infinity };

  /** Throws when the plan has no stage or its servers cannot share their connections. */
  constructor(options: RunOptions) {
    const { servers } = options;
    if (!servers.length || servers.length > 4 || new Set(servers.map(({ server }) => server.id)).size < servers.length)
      throw new Error("Select one to four different servers");
    this.#servers = servers.map((prepared) => {
      const id = prepared.server.id;
      return {
        id,
        prepared,
        latency: new ServerLatency(),
        buckets: new LatencyBuckets((bucket) => this.#emit({ type: "latency", serverId: id, bucket })),
        removed: false,
        rotated: false,
        continuity: 0,
        bus: { current: null },
      };
    });
    this.#source = this.#servers.find((server) => server.id === options.focus) ?? this.#servers[0]!;
    this.#dropped = options.dropped;
    this.#open = options.open;
    this.#streams = options.streams;
    this.#online = options.online ?? (() => navigator.onLine !== false);
    this.#time = options.clock ?? pageClock;
    this.#validate(options.plan, this.#servers);
    this.#plan = options.plan;
    this.#segments = timeline(options.plan);
  }

  get plan(): RunPlan {
    return this.#plan;
  }

  on(handler: (event: RunEvent) => void): () => void {
    this.#handlers.add(handler);
    return () => this.#handlers.delete(handler);
  }

  start(): void {
    if (this.#state !== "created") return;
    this.#state = "running";
    this.#clock = new RunClock(this.#time);
    this.#tickAt = this.#clock.start;
    const first = this.#segments[0]!;
    this.#emit({ type: "phase", phase: "connecting", stage: first.activity.stage, t: 0, step: "preparing" });
    // A server that failed its start check counts against the first stage that moves bytes.
    const charged = (this.#segments.find(({ activity }) => activity.transfer.length) ?? first).activity.stage;
    for (const { server, reason, message } of this.#dropped)
      this.#record(server.id, "throughput", reason, message, charged);
    this.#tick();
    this.#arm();
  }

  /** The user's stop: nothing is reported or saved. */
  abort(): void {
    if (this.#state !== "running") return;
    this.#state = "aborted";
    for (const server of this.#servers) server.buckets.flush(this.#elapsed);
    this.#release();
    this.#emit({ type: "aborted", t: this.#elapsed });
  }

  /** Ends now with what was measured; each stage left unfinished fails for `reason` on each of `servers`. */
  end(reason: FailureReason, message: string, servers: readonly { id: string }[] = this.#participants()): void {
    if (this.#state !== "running") return;
    for (const stage of STAGES)
      if (planned(this.#plan, stage) && !this.#settled.has(stage))
        for (const { id } of servers) this.#record(id, failureScope(stage), reason, message, stage);
    this.#finish();
  }

  /** Whether a stage may still be turned on or off. */
  canChange(stage: Stage): boolean {
    return this.#state !== "running" || (!this.#finishing && changeable(this.#stage?.activity.stage ?? null, stage));
  }

  /** Reshapes what has not run yet; throws (changing nothing) when the plan is impossible. */
  reconfigure(change: LiveChange): void {
    if (this.#state !== "running") return;
    const plan = { ...this.#plan, ...change };
    for (const stage of STAGES)
      if (planned(plan, stage) !== planned(this.#plan, stage) && !this.canChange(stage))
        throw new Error(`The ${stage} stage can no longer change`);
    this.#validate(plan, this.#participants());
    const stage = this.#stage;
    this.#plan = plan;
    // A stage that ended, or is ending, keeps its segment: the change reshapes only what follows.
    const ended = stage?.phase === "finalizing" || stage?.phase === "settled";
    this.#segments = replan(this.#segments, this.#elapsed, plan, ended);
    const shown = this.#shown;
    const now = segmentAt(this.#segments, this.#elapsed);
    const continued = !ended && shown && now?.phase === shown.phase && now.activity.stage === shown.activity.stage;
    // An early finish stays armed only on the segment it armed on.
    if (stage && !continued) stage.armedAt = null;
    if (continued) {
      this.#shown = now;
      for (const server of this.#servers) server.buckets.widen(now.end - now.start);
      if (stage) this.#evaluate(stage);
    }
    this.#tick();
  }

  details(): MultiServerResult {
    return {
      selection: [
        ...this.#servers.map(({ prepared }) => prepared.server),
        ...this.#dropped.map(({ server }) => server),
      ],
      participants: this.#participants().map(({ id }) => id),
      latencyFocus: this.#focus().id,
      servers: this.#servers.map((server) => this.#summary(server)),
      intervals: structuredClone(this.#aggregate.intervals),
      omittedIntervals: this.#aggregate.omittedIntervals,
      failures: [...this.#failures],
    };
  }

  /** Completes after the current stage; later stages do not run. */
  #finish(): void {
    this.#finishing = true;
    this.#cancelTimer();
    const stage = this.#stage;
    // A finalizing stage completes the run as it settles.
    if (!stage || stage.phase === "settled") this.#complete();
    else void this.#endStage(stage);
  }

  #emit(event: RunEvent): void {
    for (const handler of this.#handlers)
      try {
        handler(event);
      } catch (error) {
        reportError(error);
      }
  }

  #validate(plan: RunPlan, servers: readonly Participant[]): void {
    if (!STAGES.some((stage) => planned(plan, stage))) throw new Error("Give at least one stage a duration");
    for (const activity of activities(plan))
      if (activity.transfer.length)
        this.#streams(
          servers.map(({ prepared }) => prepared),
          activity,
        );
  }

  #participants(): Participant[] {
    return this.#servers.filter((server) => !server.removed);
  }

  /** Servers whose idle population is still running. */
  #latencyParticipants(): Participant[] {
    return this.#participants().filter((server) => server.prepared.latency && !server.latency.failed.has("latency"));
  }

  /** The latency source, or once it left, the first remaining server that measured idle latency. */
  #focus(): Participant {
    const source = this.#source;
    if (!source.removed) return source;
    const measured = (server: Participant) => !!server.latency.result();
    return this.#latencyParticipants().find(measured) ?? this.#servers.find(measured) ?? source;
  }

  #current(stage: StageRun): boolean {
    return this.#state === "running" && this.#stage === stage;
  }

  /** Timeline ms now: the last tick plus the page time since, unless held. */
  #position(): number {
    return this.#elapsed + (this.#clock.held ? 0 : Math.max(0, this.#clock.read() - this.#tickAt));
  }

  #arm(): void {
    if (this.#timer || this.#state !== "running" || this.#clock.held) return;
    let ms = TICK_MS;
    const segment = segmentAt(this.#segments, this.#elapsed);
    if (segment) ms = Math.min(ms, segment.end - this.#elapsed);
    // Buckets close on time without a later probe.
    for (const { buckets } of this.#servers)
      if (buckets.nextEnd !== null) ms = Math.min(ms, buckets.nextEnd - this.#elapsed);
    this.#timer = this.#time.after(Math.max(1, ms), () => {
      this.#timer = null;
      this.#tick();
      this.#arm();
    });
  }

  #cancelTimer(): void {
    this.#timer?.();
    this.#timer = null;
  }

  #tick(): void {
    if (this.#state !== "running") return;
    const now = this.#clock.read();
    const sinceTick = now - this.#tickAt;
    this.#tickAt = now;
    if (this.#clock.held) return;
    // One tick never crosses the current segment's end, so a suspended page still enters every segment in order.
    const current = segmentAt(this.#segments, this.#elapsed);
    this.#elapsed = Math.min(this.#elapsed + Math.max(0, sinceTick), current?.end ?? Infinity);
    const stage = this.#stage;
    if (stage) {
      this.#resumeAfterGap(stage);
      this.#expire(stage);
      if (this.#state !== "running" || this.#clock.held) return;
    }
    for (const { buckets } of this.#servers) buckets.closeThrough(this.#elapsed);
    if (stage?.phase === "measuring" && now - this.#sampledAt >= TICK_MS - 1) this.#sample(stage, now);
    if (this.#elapsed >= (this.#segments.at(-1)?.end ?? 0))
      return stage && stage.phase !== "settled" ? void this.#endStage(stage) : this.#complete();
    const segment = segmentAt(this.#segments, this.#elapsed);
    if (!segment) return;
    if (stage?.armedAt != null && this.#evaluate(stage)) return this.#tick();
    if (segment !== this.#shown) return this.#enter(segment);
    const budgetMs = segment.end - segment.start;
    const key = `${segment.start}:${budgetMs}`;
    if (key === this.#progress.key && now - this.#progress.at < PROGRESS_CADENCE_MS) return;
    this.#progress = { key, at: now };
    this.#emit({ type: "progress", elapsedMs: this.#elapsed - segment.start, budgetMs });
  }

  #enter(segment: Segment): void {
    const shown = this.#shown;
    if (!shown || !this.#stage) return this.#prepare(segment);
    if (shown.activity.stage !== segment.activity.stage) return void this.#endStage(this.#stage);
    // A warmup hands over to its window: the same resources, nothing to wait for.
    this.#show(segment, "running");
    this.#measure(this.#stage);
  }

  #show(segment: Segment, step: "preparing" | "running" | "finalizing"): void {
    this.#shown = segment;
    this.#progress = { key: "", at: -Infinity };
    this.#emit({ type: "phase", phase: segment.phase, stage: segment.activity.stage, t: segment.start, step });
  }

  /** Opens every participant's resources for the segment's stage; the timeline waits until they work. */
  #prepare(segment: Segment): void {
    const first = !this.#stage;
    const { activity } = segment;
    let streams: StageRun["streams"];
    // A sole server retries at each new stage.
    if (this.#servers.length === 1) this.#servers[0]!.removed = false;
    try {
      streams = this.#streams(
        this.#participants().map(({ prepared }) => prepared),
        activity,
      );
    } catch (cause) {
      return this.#fail(
        classify(cause, "protocol-error", this.#online()),
        cause instanceof Error ? cause.message : "Stage preparation failed",
      );
    }
    const stage: StageRun = {
      activity,
      phase: "preparing",
      abort: new AbortController(),
      seats: new Map(),
      streams,
      measured: false,
      closedAt: 0,
      disturbed: false,
      armedAt: null,
      evaluatedAt: -Infinity,
      finishedEarly: false,
      plan: null,
    };
    this.#stage = stage;
    this.#stages.set(activity.stage, stage);
    this.#clock.hold();
    // The first stage shows as connecting until it works; a later one shows at once, without a countdown.
    if (first) this.#shown = segment;
    else this.#show(segment, "preparing");
    // A phase handler may have ended the run.
    if (!this.#current(stage) || stage.phase !== "preparing") return;
    const servers = this.#participants().filter((server) => activity.stage !== "latency" || server.prepared.latency);
    for (const server of this.#participants()) server.latency.failed.delete(activity.stage);
    const opened = servers.map(async (server) => {
      const seat = this.#seat(stage, server);
      await seat.resources.prepare(stage.abort.signal);
    });
    void Promise.allSettled(opened).then((results) => {
      if (!this.#current(stage) || stage.phase !== "preparing") return;
      results.forEach((result, index) => {
        if (result.status === "fulfilled" || !this.#current(stage) || stage.phase !== "preparing") return;
        // A grant withdrawn between stages surfaces here first, wrapped by whatever it refused.
        const signIn = [...causes(result.reason)].find((error) => error instanceof SignInRequired);
        const error = signIn ?? result.reason;
        this.#remove(
          servers[index]!,
          signIn ? "sign-in-required" : classify(result.reason, "preparation-failed", this.#online()),
          error instanceof Error ? error.message : "Measurement preparation failed",
        );
      });
      if (!this.#current(stage) || stage.phase !== "preparing") return;
      this.#tickAt = this.#clock.resume();
      const shown = this.#shown!;
      this.#show(shown, "running");
      stage.phase = "warmup";
      if (shown.phase !== "warmup") this.#measure(stage);
      this.#tick();
      this.#arm();
    });
  }

  #seat(stage: StageRun, server: Participant): Seat {
    const { activity } = stage;
    // Resources report only after `open` returns them.
    const seat = {
      server,
      down: 0,
      up: null,
      progressAt: { down: 0, up: 0 },
      heardAt: 0,
      grewNanos: 0,
      recovery: null,
      latencyStall: null,
      summaryAt: -Infinity,
    } as Partial<Seat> as Seat;
    seat.resources = this.#open({
      server: server.prepared,
      activity,
      ...(stage.streams.get(server.id) ?? { streams: { down: 0, up: 0 } }),
      cadence: activity.stage === "latency" ? this.#plan.pingCadence : this.#plan.loadedPingCadence,
      seed: `r${Math.round(this.#clock.start)}`,
      events: this.#events(stage, seat),
      now: () => this.#clock.runMs(),
      bus: server.bus,
    });
    stage.seats.set(server.id, seat);
    return seat;
  }

  /** The measured window opens: evidence counts from now on. */
  #measure(stage: StageRun): void {
    const seats = active(stage);
    stage.phase = "measuring";
    if (!seats.length) return;
    stage.measured = true;
    stage.disturbed = false;
    const { activity } = stage;
    const shown = this.#shown!;
    const cadence = activity.stage === "latency" ? this.#plan.pingCadence : this.#plan.loadedPingCadence;
    const at = this.#clock.activeMs();
    for (const seat of seats) {
      seat.server.buckets.reset(
        this.#elapsed,
        activity.stage,
        this.#continuity,
        shown.end - shown.start,
        CADENCE_MS[cadence],
      );
      if (activity.stage === "latency") seat.server.latency.idle.reset();
      seat.progressAt = { down: at, up: at };
      seat.heardAt = at;
      seat.resources.measure();
    }
    if (activity.stage === "latency") return;
    // The live rate and the first upload window start at the window, not in the warmup.
    this.#live.reset(this.#counts(stage), this.#clock.read());
    this.#aggregate.begin(
      activity.stage,
      seats.map(({ server }) => server.id),
      this.#clock.runMs(),
    );
    if (activity.transfer.includes("up")) {
      for (const seat of seats) seat.up = null;
      const answers = seats.map(async (seat) => {
        const checkpoint = await seat.resources.checkpoint(stage.abort.signal).catch(() => null);
        // A feed record heard while the answer was in flight is newer; the answer would take the count back.
        if (!checkpoint || seat.up || stage.phase !== "measuring" || !this.#current(stage)) return;
        this.#hear(seat, checkpoint);
        this.#live.receiver(seat.server.id, checkpoint, this.#clock.read());
      });
      void Promise.allSettled(answers).then(() => {
        if (stage.phase === "measuring" && this.#current(stage) && this.#boundary(stage)) this.#tick();
      });
    }
    this.#boundary(stage);
  }

  /** The callbacks one server's resources make in one stage; inert once the stage closes or the server leaves. */
  #events(stage: StageRun, seat: Seat): StageEvents {
    const { server } = seat;
    const name = stage.activity.stage;
    const open = () => this.#current(stage) && stage.phase !== "settled";
    const live = () => open() && !server.removed;
    const measuring = () => stage.phase === "measuring" && live();
    // A leaving server's probe accounting still counts: its population reports what was lost.
    const counting = () => stage.measured && open();
    // Tearing down lanes that moved to the window's end cannot fail the stage; a server that stopped earlier can.
    const teardown = () =>
      stage.phase === "finalizing" &&
      (!stage.measured || stage.activity.transfer.every((dir) => stage.closedAt - seat.progressAt[dir] < QUIET_MS));
    return {
      download: (bytes) => {
        if (!measuring() || !stage.activity.transfer.includes("down") || !(bytes > 0)) return;
        seat.down += bytes;
        seat.progressAt.down = this.#clock.activeMs();
        this.#aggregate.addDownload(name as TransferStage, server.id, bytes);
      },
      receiver: (checkpoint) => {
        if (!measuring()) return;
        // The first record after a page gap belongs to the interval the gap opens.
        this.#resumeAfterGap(stage);
        this.#hear(seat, checkpoint);
        if (!this.#stalled) this.#live.receiver(server.id, checkpoint, this.#clock.read());
        if (this.#boundary(stage)) this.#tick();
      },
      probes: (outcomes) => {
        if (!counting() || server.removed) return;
        const position = this.#position();
        for (const outcome of outcomes) {
          const t = this.#timelineAt(outcome.observedAt, position);
          server.latency.observe(name, outcome, t, server.continuity);
          if (outcome.inWindow) server.buckets.observe(t, outcome.rttMs, outcome.timedOut);
        }
        const now = this.#clock.read();
        if (now - seat.summaryAt >= SUMMARY_CADENCE_MS) {
          seat.summaryAt = now;
          this.#emit({ type: "summary", serverId: server.id, stage: name, summary: server.latency.summary(name) });
        }
        if (name === "latency" && now - stage.evaluatedAt >= STABILITY_CADENCE_MS && this.#evaluate(stage))
          this.#tick();
      },
      probesInterrupted: (count, reason) => {
        if (counting()) server.latency.stages[name].interrupt(count, reason);
      },
      probesIncomplete: () => {
        if (!counting()) return;
        server.latency.stages[name].markIncomplete();
        this.#failure(server, "latency", "connection-lost", "Latency observations were interrupted");
      },
      stalled: (stall) => {
        if (!live()) return;
        if (name === "latency") return this.#latencyStall(seat, stall.detail || "Latency interrupted");
        if (stage.phase !== "measuring") return;
        if (!seat.recovery) {
          seat.recovery = { abort: new AbortController(), stall };
          this.#live.restart(server.id, seat.down, this.#clock.read());
          this.#disturb(stage);
          this.#updateStalled();
        }
        // An unknown upload id grants one replacement receiver per server and run, even mid-recovery.
        if (stall.rotate && stall.direction === "up" && !server.rotated) {
          server.rotated = true;
          seat.resources.replaceUpload(seat.recovery.abort.signal).catch(() => {});
        }
      },
      resumed: () => {
        if (!live()) return;
        if (name === "latency") {
          seat.latencyStall = null;
          return this.#updateStalled();
        }
        if (!seat.recovery) return;
        this.#live.restart(server.id, seat.down, this.#clock.read());
        seat.recovery.abort.abort();
        seat.recovery = null;
        this.#updateStalled();
      },
      latencyStalled: (detail) => {
        server.continuity++;
        if (live()) this.#latencyStall(seat, detail);
      },
      latencyResumed: () => {
        if (!live()) return;
        seat.latencyStall = null;
        this.#updateStalled();
      },
      failed: (reason, message) => {
        if (live() && !teardown()) this.#remove(server, reason, message);
      },
      signInRequired: (role) => {
        if (!live() || teardown()) return;
        const message = `Sign in to ${server.prepared.server.name}`;
        // Loaded latency alone never removes a throughput participant.
        if (role === "throughput" || name === "latency") this.#remove(server, "sign-in-required", message);
        else this.#failure(server, "latency", "sign-in-required", message);
      },
      uploadHint: (lane, bytes, elapsedMs) => {
        if (live()) this.#live.hint(server.id, lane, bytes, elapsedMs, this.#clock.read());
      },
    };
  }

  /** A page-clock observation on the timeline, relative to the last tick rather than to its delivery. */
  #timelineAt(observedAt: number, position = this.#position()): number {
    return Math.max(this.#shown?.start ?? 0, Math.min(position, this.#elapsed + observedAt - this.#tickAt));
  }

  /** Grown receiver bytes are progress; any record with advancing receiver time shows the feed is alive. */
  #hear(seat: Seat, checkpoint: Checkpoint): void {
    const last = seat.up;
    const fresh = !last || last.id !== checkpoint.id;
    const at = this.#clock.activeMs();
    if (fresh || checkpoint.nanos > last.nanos) seat.heardAt = at;
    if (fresh || checkpoint.bytes > last.bytes) {
      seat.progressAt.up = at;
      seat.grewNanos = checkpoint.nanos;
    }
    seat.up = checkpoint;
  }

  #counts(stage: StageRun): Map<string, number> {
    return new Map(active(stage).map((seat) => [seat.server.id, seat.down]));
  }

  #snapshot(stage: StageRun): Boundary {
    const boundary: Boundary = { atMs: this.#clock.runMs(), down: {}, up: {} };
    for (const seat of active(stage)) {
      boundary.down[seat.server.id] = seat.down;
      boundary.up[seat.server.id] = seat.up;
    }
    return boundary;
  }

  /** Observes every participant's evidence now; true when that confirmed an early finish. */
  #boundary(stage: StageRun): boolean {
    if (stage.phase !== "measuring" || !stage.activity.transfer.length || !this.#current(stage)) return false;
    // Resuming observes a boundary of its own, and a gap rules out finishing early.
    if (this.#resumeAfterGap(stage)) return false;
    return this.#observe(stage, this.#snapshot(stage));
  }

  #observe(stage: StageRun, boundary: Boundary, final = false): boolean {
    const interval = this.#aggregate.current?.id;
    const sample = this.#aggregate.observe(boundary, final);
    // A replaced receiver or a regressed counter began a new interval.
    if (interval !== this.#aggregate.current?.id) this.#disturb(stage);
    return !!sample && !final && !this.#stalled && this.#evaluate(stage);
  }

  /** Unique measured bytes of the run so far, both directions and every server. */
  #bytes(): number {
    let bytes = 0;
    for (const { id } of this.#servers) {
      const totals = this.#aggregate.totals(id);
      bytes += totals.down + totals.up;
    }
    return bytes;
  }

  /** The presentation cadence: download boundaries and live rates, which results never read. */
  #sample(stage: StageRun, now: number): void {
    this.#sampledAt = now;
    const { transfer } = stage.activity;
    if (transfer.includes("down")) this.#boundary(stage);
    if (stage.activity.stage === "latency" || !this.#current(stage)) return;
    const seats = active(stage);
    if (!this.#stalled) for (const seat of seats) this.#live.download(seat.server.id, seat.down, now);
    const rate = (dir: Direction) => (!transfer.includes(dir) ? null : this.#stalled ? 0 : this.#live.rate(dir));
    const lanes = (id: string) =>
      this.#servers.find((server) => server.id === id)?.prepared.throughput.target.transport === "fetch-stream"
        ? (stage.streams.get(id)?.streams.up ?? 0)
        : 1;
    const quietMs =
      this.#clock.activeMs() - Math.max(...seats.flatMap((seat) => transfer.map((dir) => seat.progressAt[dir])));
    this.#emit({
      type: "live",
      sample: {
        t: Math.min(this.#position(), this.#shown!.end),
        stage: stage.activity.stage,
        continuityId: this.#continuity,
        bytes: this.#bytes(),
        down: rate("down"),
        up: rate("up"),
        bridgedUp: this.#stalled || !transfer.includes("up") ? null : this.#live.bridgedUpload(now, lanes),
        stalled: this.#stalled,
        quietMs: this.#stalled && quietMs >= QUIET_MS ? quietMs : null,
      },
    });
  }

  /** Silence and stall budgets count active time only, so a page gap is never a server's stall. */
  #expire(stage: StageRun): void {
    const at = this.#clock.activeMs();
    for (const seat of active(stage)) {
      if (seat.server.removed) continue;
      if (seat.latencyStall && at - seat.latencyStall.at >= LATENCY_RECOVERY_MS)
        this.#failLatency(stage, seat, seat.latencyStall.detail);
      if (stage.phase !== "measuring") continue;
      if (stage.activity.transfer.some((dir) => at - seat.progressAt[dir] >= QUIET_MS)) stage.disturbed = true;
      // Silence the others share is the link's: everyone stays, and the stage end judges who is still silent.
      const dir = silence(stage, seat, at);
      if (dir && active(stage).some((other) => other !== seat && at - other.progressAt[dir] < QUIET_MS))
        this.#leave(seat, dir);
    }
    this.#updateStalled();
  }

  /** The run is stalled while every participant is recovering or has moved nothing for a moment. */
  #updateStalled(): void {
    const stage = this.#stage;
    if (!stage || !this.#current(stage) || stage.phase === "finalizing" || stage.phase === "settled") return;
    const latency = stage.activity.stage === "latency";
    const at = this.#clock.activeMs();
    const seats = active(stage).filter((seat) => !latency || !seat.server.latency.failed.has("latency"));
    const stalled = seats.length > 0 && seats.every((seat) => recovering(stage, seat, at) || quiet(stage, seat, at));
    if (stalled === this.#stalled) return;
    this.#stalled = stalled;
    this.#breakContinuity();
    if (!stalled) {
      this.#live.reset(this.#counts(stage), this.#clock.read());
      return this.#emit({ type: "stall", stall: null });
    }
    stage.disturbed = true;
    const reported = seats.find((seat) => seat.recovery)?.recovery?.stall;
    this.#emit({
      type: "stall",
      stall:
        reported ??
        (latency ? { reason: "connection-lost", detail: "Latency interrupted" } : { reason: "timeout", detail: "" }),
    });
  }

  #latencyStall(seat: Seat, detail: string): void {
    const stage = this.#stage!;
    if (seat.server.latency.failed.has(stage.activity.stage) || seat.latencyStall) return;
    seat.latencyStall = { at: this.#clock.activeMs(), detail };
    this.#updateStalled();
  }

  /** A population that stayed down too long fails; loaded latency alone never removes a throughput participant. */
  #failLatency(stage: StageRun, seat: Seat, detail: string): void {
    if (stage.activity.stage === "latency") return this.#remove(seat.server, "connection-lost", detail);
    seat.latencyStall = null;
    seat.server.latency.fail(stage.activity.stage);
    this.#failure(seat.server, "latency", "connection-lost", detail);
  }

  /** A server leaves with the failure it last reported, else as silent in `dir`. */
  #leave(seat: Seat, dir?: Direction): void {
    const stall = seat.recovery?.stall;
    const direction = stall?.direction ?? dir ?? this.#stage?.activity.transfer[0] ?? "down";
    this.#remove(seat.server, stall?.reason ?? "timeout", stall?.detail || `${direction} direction carried no data`);
  }

  #remove(server: Participant, reason: FailureReason, message: string): void {
    const stage = this.#stage;
    if (server.removed || !stage || !this.#current(stage)) return;
    const seat = stage.seats.get(server.id);
    this.#disturb(stage);
    if (stage.activity.stage === "latency") {
      server.latency.fail("latency");
      if (seat) seat.latencyStall = null;
      this.#failure(server, "latency", reason, message);
      // An unreachable server would hold each later stage's preparation until it timed out.
      if ((reason === "connection-lost" || reason === "timeout") && this.#participants().length > 1) {
        server.removed = true;
        seat?.resources.discard(true);
      }
      this.#updateStalled();
      if (!this.#latencyParticipants().length) this.#skip(stage);
      return;
    }
    server.removed = true;
    if (seat) {
      seat.recovery?.abort.abort();
      seat.recovery = seat.latencyStall = null;
      // Failed-stage shutdown accounts for buffered probes before the worker goes.
      seat.resources.discard(true);
    }
    this.#live.drop(server.id);
    this.#failure(server, "throughput", reason, message);
    this.#updateStalled();
    const survivors = this.#participants();
    if (!survivors.length) {
      if (![...this.#stages.values()].some(({ measured }) => measured))
        return this.#fail(reason, `All selected servers failed. ${server.prepared.server.name}: ${message}`);
      // A sole server skips to its next stage; several that all fail end the run as incomplete.
      return this.#servers.length === 1
        ? this.#skip(stage)
        : this.end(reason, "No server was left to run this stage", this.#servers);
    }
    if (stage.phase === "measuring" && stage.activity.transfer.length)
      this.#aggregate.dropout(
        survivors.map(({ id }) => id),
        this.#clock.runMs(),
      );
    if (this.#boundary(stage)) this.#tick();
  }

  /** With nobody left to measure, the stage ends now and keeps whatever its evidence supports. */
  #skip(stage: StageRun): void {
    if (stage.phase === "finalizing" || stage.phase === "settled") return;
    for (const segment of this.#segments)
      if (segment.activity.stage === stage.activity.stage) this.#elapsed = Math.max(this.#elapsed, segment.end);
    stage.armedAt = null;
    queueMicrotask(() => this.#tick());
  }

  /** A page gap starts a new interval before any evidence after it applies, so no headline, early finish or live rate
      spans it; true when one did. */
  #resumeAfterGap(stage: StageRun): boolean {
    this.#clock.read();
    if (!this.#clock.takeGap() || stage.phase !== "measuring") return false;
    this.#resumeEvidence(stage);
    return true;
  }

  /** After a page gap, evidence resumes in a new interval; the one before it still counts. */
  #resumeEvidence(stage: StageRun): void {
    this.#disturb(stage);
    this.#live.reset(this.#counts(stage), this.#clock.read());
    this.#breakContinuity();
    if (stage.activity.stage === "latency") return;
    for (const seat of active(stage)) seat.up = null;
    this.#aggregate.begin(
      stage.activity.stage,
      active(stage).map(({ server }) => server.id),
      this.#clock.runMs(),
      "evidence-resumed",
    );
    this.#boundary(stage);
  }

  /** A stall, dropout or gap restarts the steadiness window and rules out finishing this stage early. */
  #disturb(stage: StageRun): void {
    stage.disturbed = true;
    stage.armedAt = null;
    this.#aggregate.resetStability();
    if (stage.activity.stage === "latency") for (const server of this.#servers) server.latency.idle.reset();
  }

  #breakContinuity(): void {
    for (const { buckets } of this.#servers) buckets.flush(this.#elapsed);
    this.#continuity++;
    for (const { buckets } of this.#servers) buckets.restart(this.#elapsed, this.#continuity);
  }

  /** Scores the measured window; true when that confirmed an early finish and cut the timeline there. */
  #evaluate(stage: StageRun): boolean {
    const segment = this.#shown;
    if (
      !segment ||
      !this.#segments.includes(segment) ||
      segment.phase === "warmup" ||
      stage.phase !== "measuring" ||
      !this.#plan.adaptive
    ) {
      stage.armedAt = null;
      return false;
    }
    const latency = segment.phase === "latency";
    const seats = latency ? active(stage).filter((seat) => !seat.server.latency.failed.has("latency")) : active(stage);
    // Completion respects the least stable and least sampled path without pooling populations.
    const scores: Confidence[] = latency ? seats.map((seat) => seat.server.latency.idle.confidence()) : [];
    const confidence = latency
      ? scores.length
        ? {
            score: Math.min(...scores.map(({ score }) => score)),
            sampleCount: Math.min(...scores.map(({ sampleCount }) => sampleCount)),
          }
        : { score: 0, sampleCount: 0 }
      : this.#aggregate.confidence();
    if (!latency) this.#aggregate.trackStable(confidence.score);
    stage.evaluatedAt = this.#clock.read();
    const at = this.#clock.activeMs();
    const eligible =
      !stage.disturbed &&
      (latency ? seats.length > 0 : this.#aggregate.sufficient) &&
      !seats.some((seat) => recovering(stage, seat, at)) &&
      mayFinishEarly({
        kind: latency ? "latency" : "transfer",
        intervalMs: CADENCE_MS[this.#plan.pingCadence],
        elapsedMs: this.#elapsed - segment.start,
        durationMs: segment.end - segment.start,
        confidence,
      });
    if (!eligible) {
      stage.armedAt = null;
      return false;
    }
    stage.armedAt ??= this.#elapsed;
    if (this.#elapsed - stage.armedAt < EARLY_FINISH.confirmationMs) return false;
    const total = this.#segments.at(-1)!.end;
    this.#segments = truncate(this.#segments, segment, this.#elapsed);
    if (this.#segments.at(-1)!.end < total) stage.finishedEarly = true;
    stage.armedAt = null;
    return true;
  }

  /** Each participant stops as soon as its own final evidence is known. */
  async #endStage(stage: StageRun): Promise<void> {
    if (stage.phase === "finalizing" || stage.phase === "settled") return;
    const preparing = stage.phase === "preparing";
    stage.phase = "finalizing";
    const name = stage.activity.stage;
    stage.plan = {
      planned: planned(this.#plan, name),
      durationMs: this.#plan.duration[`${name}Ms`],
      early: stage.finishedEarly,
    };
    stage.closedAt = this.#clock.activeMs();
    this.#clock.hold();
    const shown = this.#shown!;
    if (!preparing) {
      // An early finish cut the shown segment short; the stage shows where it ended.
      const segment =
        this.#segments.find((each) => each.phase === shown.phase && each.activity === shown.activity) ?? shown;
      this.#show(segment, "finalizing");
      const budgetMs = segment.end - segment.start;
      this.#emit({ type: "progress", elapsedMs: Math.min(budgetMs, this.#elapsed - segment.start), budgetMs });
    }
    const finishing = new Map<Seat, Promise<void>>();
    const finish = (seat: Seat) => {
      if (!finishing.has(seat))
        finishing.set(
          seat,
          seat.resources.finish().catch(() => {}),
        );
    };
    if (stage.measured && stage.activity.transfer.length) await this.#finalBoundary(stage, finish);
    if (!this.#current(stage)) return;
    for (const seat of active(stage)) if (seat.latencyStall) this.#failLatency(stage, seat, seat.latencyStall.detail);
    for (const seat of active(stage)) finish(seat);
    await Promise.all(finishing.values());
    if (!this.#current(stage)) return;
    stage.phase = "settled";
    for (const { buckets } of this.#servers) buckets.flush(this.#elapsed);
    for (const seat of stage.seats.values()) {
      seat.recovery?.abort.abort();
      seat.recovery = seat.latencyStall = null;
      const summary = seat.server.latency.summary(name);
      this.#emit({ type: "summary", serverId: seat.server.id, stage: name, summary });
      if (!this.#current(stage)) return;
    }
    if (this.#stalled) {
      this.#stalled = false;
      this.#emit({ type: "stall", stall: null });
      if (!this.#current(stage)) return;
    }
    const status = this.#settle(name);
    this.#emit({ type: "stageEnd", stage: name, status, results: { ...this.#results } });
    // A handler may have ended or stopped the run.
    if (!this.#current(stage)) return;
    this.#tickAt = this.#clock.resume();
    // A live change during the stage end may have replaced or removed the next segment.
    const next = segmentAt(this.#segments, this.#elapsed);
    if (this.#finishing || !next) return this.#complete();
    this.#prepare(next);
  }

  /** Terminal evidence after this boundary cannot change measured totals or rates. */
  async #finalBoundary(stage: StageRun, finish: (seat: Seat) => void): Promise<void> {
    const seats = active(stage);
    const upload = stage.activity.transfer.includes("up");
    const boundary = this.#snapshot(stage);
    const at = stage.closedAt;
    // Judged as the window closed: a final checkpoint answered late over a full uplink must not make a server that
    // was moving look silent.
    const silent = new Set(seats.filter((seat) => silence(stage, seat, at)));
    const retrying = new Set(seats.filter((seat) => recovering(stage, seat, at)));
    const answers = await Promise.allSettled(
      seats.map((seat) =>
        (upload ? seat.resources.checkpoint(stage.abort.signal, true) : Promise.resolve(null)).finally(() =>
          finish(seat),
        ),
      ),
    );
    if (!this.#current(stage)) return;
    seats.forEach((seat, index) => {
      if (!upload) return;
      const answer = answers[index]!;
      const checkpoint = answer.status === "fulfilled" ? answer.value : null;
      if (checkpoint) this.#hear(seat, checkpoint);
      boundary.up[seat.server.id] = checkpoint;
    });
    this.#observe(stage, boundary, true);
    seats.forEach((seat, index) => {
      const answer = answers[index]!;
      if (answer.status === "rejected" && answer.reason instanceof SignInRequired)
        this.#remove(seat.server, "sign-in-required", answer.reason.message);
    });
    // Whoever is still silent, or still retrying a failure, leaves here as it would have mid-stage.
    for (const seat of seats) {
      const dir = silent.has(seat) ? silence(stage, seat, at) : undefined;
      if (dir || (retrying.has(seat) && recovering(stage, seat, this.#clock.activeMs()))) this.#leave(seat, dir);
    }
    this.#aggregate.dropout(
      active(stage).map(({ server }) => server.id),
      this.#clock.runMs(),
    );
    this.#aggregate.close();
  }

  #failed(stage: Stage, serverId?: string): boolean {
    const scope = failureScope(stage);
    return this.#failures.some(
      (failure) => failure.stage === stage && failure.scope === scope && (!serverId || failure.serverId === serverId),
    );
  }

  /** A stage's results and status are reduced once, the moment it ends. */
  #settle(name: Stage): StageStatus {
    const settled = this.#settled.get(name);
    if (settled) return settled.status;
    const stage = this.#stages.get(name);
    const plan = stage?.plan ?? {
      planned: planned(this.#plan, name),
      durationMs: this.#plan.duration[`${name}Ms`],
      early: false,
    };
    if (!stage && !plan.planned) {
      this.#settled.set(name, { status: "not-run", plan });
      return "not-run";
    }
    if (stage) this.#reduce(name, plan.early);
    const measured = lanes(this.#results, name).every(Boolean);
    // A result that spans too little of its planned time is kept, never complete: the rule History checks.
    const short =
      measured &&
      name !== "latency" &&
      coveredMs(this.#aggregate.intervals, name) < plan.durationMs * minCoverage(plan.early);
    if (stage && !this.#failed(name) && (short || !measured)) {
      const ids =
        name === "latency"
          ? [this.#focus().id]
          : (this.#aggregate.intervals.findLast((interval) => interval.stage === name)?.participants ??
            this.#participants().map(({ id }) => id));
      const message = short ? "Measured too little of the planned time" : "Too little measured evidence for a result";
      for (const id of ids) this.#record(id, failureScope(name), "insufficient-evidence", message, name);
    }
    const status = stageStatus(lanes(this.#results, name), this.#failed(name));
    this.#settled.set(name, { status, plan });
    return status;
  }

  #reduce(name: Stage, early: boolean): void {
    if (name === "latency") {
      this.#results.latency = this.#focus().latency.result();
      return;
    }
    const { down, up } = this.#aggregate.settle(name, early);
    if (name === "bidirectional") {
      const both = down && up && this.#wire([...(down.window.down ?? []), ...(up.window.up ?? [])]);
      this.#results.bidirectional = { down: down?.result ?? null, up: up?.result ?? null, wire: both || null };
      return;
    }
    const lane = name === "download" ? down : up;
    const components = lane?.window[name === "download" ? "down" : "up"];
    this.#results[name] = lane && { ...lane.result, wire: this.#wire(components ?? []) };
  }

  /** The wire estimate of a window: each server's share on its own path; none when any share is too short or its
      path is unknown. */
  #wire(components: readonly ComponentWindow[]): WireModel | null {
    const paths: { bytesPerSec: number; path: WirePath }[] = [];
    for (const component of components) {
      const throughput = this.#servers.find(({ id }) => id === component.serverId)?.prepared.throughput;
      if (!throughput || component.durationMs < 800) return null;
      const { target, browserProtocol, probe } = throughput;
      if (target.transport === "fetch-stream" && !browserProtocol) return null;
      paths.push({
        bytesPerSec: component.bytesPerSec,
        path: {
          protocol: browserProtocol ? HTTP[browserProtocol] : undefined,
          secure: target.origin.startsWith("https://"),
          ipVersion: probe.clientIpVersion,
          transport: target.transport,
        },
      });
    }
    return paths.length ? combinedModel(paths) : null;
  }

  #record(serverId: string, scope: Scope, reason: FailureReason, message: string, stage: Stage): ServerFailure | null {
    if (this.#failures.some((f) => f.serverId === serverId && f.stage === stage && f.scope === scope)) return null;
    // Server-supplied detail is bounded and filtered before it can reach saved history.
    const failure = { serverId, stage, atMs: this.#clock.runMs(), scope, reason, message: safeDetail(message) };
    this.#failures.push(failure);
    return failure;
  }

  #failure(server: Participant, scope: Scope, reason: FailureReason, message: string): void {
    const stage = this.#stage?.activity.stage;
    const failure = stage && this.#record(server.id, scope, reason, message, stage);
    if (failure) this.#emit({ type: "failure", failure });
  }

  #complete(): void {
    if (this.#state !== "running") return;
    this.#state = "completed";
    const durationMs = this.#clock.runMs();
    const stages = Object.fromEntries(STAGES.map((stage) => [stage, this.#settle(stage)])) as Record<
      Stage,
      StageStatus
    >;
    const plans = Object.fromEntries(STAGES.map((stage) => [stage, this.#settled.get(stage)!.plan])) as StagePlans;
    const focus = this.#focus().latency;
    // The latency headline, its stages and the added latency all come from the one focus the result names.
    if (this.#stages.has("latency")) {
      this.#results.latency = focus.result();
      stages.latency = stageStatus([this.#results.latency], this.#failed("latency"));
    }
    const result: RunResult = {
      ...this.#results,
      multiServer: this.details(),
      outcome: outcomeOf(Object.values(stages), this.#failures.length),
      stages,
      latencyByStage: focus.summaries(),
      addedLatency: focus.addedLatency(),
      startedAt: this.#clock.startedAt,
      durationMs,
    };
    this.#release();
    this.#emit({ type: "complete", result, plans });
  }

  #fail(reason: FailureReason, message: string): void {
    if (this.#state !== "running") return;
    this.#state = "failed";
    for (const { buckets } of this.#servers) buckets.flush(this.#elapsed);
    this.#release();
    this.#emit({ type: "error", reason, message });
  }

  #release(): void {
    this.#cancelTimer();
    const stage = this.#stage;
    if (stage) {
      stage.abort.abort();
      for (const seat of stage.seats.values()) {
        seat.recovery?.abort.abort();
        seat.recovery = seat.latencyStall = null;
        if (stage.phase !== "settled") seat.resources.discard();
      }
    }
    this.#aggregate.close();
    for (const server of this.#servers) {
      server.latency.close();
      server.bus.current?.dispose();
      server.bus.current = null;
    }
  }

  #summary(server: Participant): ServerMeasurementSummary {
    const { id, prepared, latency } = server;
    const { throughput } = prepared;
    const result = (stage: TransferStage, dir: Direction) => this.#aggregate.serverResult(stage, dir, id);
    const evidence = {
      latency: latency.result(),
      download: result("download", "down"),
      upload: result("upload", "up"),
      bidirectional: this.#stages.has("bidirectional")
        ? { down: result("bidirectional", "down"), up: result("bidirectional", "up") }
        : null,
    };
    const status = (stage: Stage): StageStatus =>
      (this.#settled.get(stage)?.status ?? "not-run") === "not-run" || (stage === "latency" && !prepared.latency)
        ? "not-run"
        : stageStatus(lanes(evidence, stage), this.#failed(stage, id));
    return {
      server: prepared.server,
      throughput: {
        origin: throughput.target.origin,
        transport: throughput.target.transport,
        protocol: throughput.fetch.protocol,
        ...(throughput.browserProtocol ? { browserProtocol: throughput.browserProtocol } : {}),
        clientIpVersion: throughput.probe.clientIpVersion,
      },
      latencyTarget: prepared.latency
        ? { origin: prepared.latency.target.origin, transport: prepared.latency.target.transport }
        : null,
      ...evidence,
      latencyByStage: latency.summaries(),
      addedLatency: latency.addedLatency(),
      totalBytes: this.#aggregate.totals(id),
      stages: Object.fromEntries(STAGES.map((stage) => [stage, status(stage)])) as Record<Stage, StageStatus>,
    };
  }
}
