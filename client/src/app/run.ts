// The run as the page sees it; only the current run writes the facts views read.
import type { LatencyBucket } from "../engine/measure/buckets";
import type { LatencySummary } from "../engine/measure/latency";
import type { Clock } from "../engine/run/clock";
import type {
  MultiServerResult,
  RunResult,
  ServerFailure,
  StagePlans,
  StageResults,
  StageStatus,
} from "../engine/run/result";
import {
  Run,
  type DroppedServer,
  type LiveSample,
  type Phase,
  type RunOptions,
  type RunPlan,
} from "../engine/run/run";
import { CHECK_FAILED } from "../engine/net/checks";
import { adaptWarmup, planned, STAGES, totalMs, type Stage } from "../engine/run/schedule";
import {
  ROLES,
  type FailureReason,
  type OpenStage,
  type PreparedServer,
  type Role,
  type Stall,
  type TestConfig,
} from "../engine/types";
import { batch, computed, effect, signal, untrack, type Read, type Signal } from "../lib/signal";
import type { PreferenceStore } from "./preferences";
import { appendThroughput, upsertLatency, type ThroughputSample } from "./series";

/** Coverage assumes the longest warmup, since the RTT that adapts it is known only after the checks. */
const SESSION_MARGIN_MS = 60_000;
const LONGEST_WARMUP_RTT_MS = Number.POSITIVE_INFINITY;

interface RunEnvironment {
  preferences: PreferenceStore;
  /** Why Start cannot run now; it shows where it arises, so a refused Start adds nothing. */
  blocker: Read<string>;
  /** Why a Start now is refused at once, said in its place until its cause clears; "" lets it run. */
  refusal: Read<string>;
  /** The page's sign-in and every grant must cover `plannedMs`; throws with the words to show. */
  authorize(plannedMs: number, signal: AbortSignal): Promise<void>;
  /** Checks the selected servers again and takes their leases, which `release` lowers; throws the words to show when one
   *  refuses `config`, or an AbortError when superseded. */
  prepare(
    signal: AbortSignal,
    config: TestConfig,
  ): Promise<{
    servers: PreparedServer[];
    dropped: DroppedServer[];
    focus: string;
    release(): void;
  }>;
  /** Whether a run of `totalMs` still fits the sign-in; a live change that lengthens the test must. */
  fits(totalMs: number): boolean;
  /** A server failed during the run, or with null the whole run did: a path that failed is checked again. */
  recheck(failure: ServerFailure | null): void;
  proved(serverId: string, roles: readonly Role[]): void;
  /** Hands a finished run to History; null withdraws the one shown. */
  finished(run: { result: RunResult; plans: StagePlans } | null): void;
  open: OpenStage;
  mint: RunOptions["mint"];
  streams: RunOptions["streams"];
  clock?: Clock;
}

export type Preparation =
  | "idle"
  | "authenticating"
  | "checking"
  | "launching"
  | "blocked"
  | "failed";
/** What the console shows; `data-phase` carries it. */
export type ShownPhase = "idle" | Phase | "complete" | "aborted" | "error";
type Step = "preparing" | "running" | "finalizing";

interface ServerSeries {
  buckets: LatencyBucket[];
  version: Signal<number>;
  summaries: Signal<Partial<Record<Stage, LatencySummary | null>>>;
}

interface LatencySeries {
  readonly buckets: readonly LatencyBucket[];
  readonly version: Read<number>;
  readonly summaries: Read<Partial<Record<Stage, LatencySummary | null>>>;
}

const NO_RESULTS: StageResults = {
  latency: null,
  download: null,
  upload: null,
  bidirectional: null,
};

export function createRun(env: RunEnvironment) {
  const { preferences } = env;
  const phase = signal<ShownPhase>("idle");
  const stage = signal<Stage | null>(null);
  const step = signal<Step | null>(null);
  const shownAt = signal(0);
  const progress = signal({ elapsedMs: 0, budgetMs: 0 });
  const stall = signal<Stall | null>(null);
  const live = signal<LiveSample | null>(null);
  const stageBytes = signal(0);
  const throughput: ThroughputSample[] = [];
  const throughputVersion = signal(0);
  const serverSeries = new Map<string, ServerSeries>();
  /** Bumped when the series of a run that went are let go. */
  const seriesReset = signal(0);
  const results = signal(NO_RESULTS);
  const statuses = signal<Partial<Record<Stage, StageStatus>>>({});
  const details = signal<MultiServerResult | null>(null);
  const result = signal<RunResult | null>(null);
  const error = signal<{ reason: FailureReason; message: string } | null>(null);
  const plan = signal<RunPlan | null>(null);
  const preparation = signal<Preparation>("idle");
  const startError = signal("");
  /** Why the latest live change was refused, until a change applies or the run goes. */
  const refusal = signal("");
  /** The servers the current or last run measured, as Start prepared them. */
  const prepared = signal<readonly PreparedServer[]>([]);

  let run: Run | null = null;
  let rttMs = 0;
  let release = () => {};
  let pending: AbortController | null = null;
  /** The start error shown is an outright refusal, which clears with its cause. */
  let refusedOutright = false;
  /** The cumulative bytes the shown stage started from, and the latest reading. */
  let stageBase = { stage: "", bytes: 0, last: 0 };
  /** The planned length of the current run, which the throughput series is compacted against. */
  let spanMs = 0;

  const running = computed(() => !["idle", "complete", "aborted", "error"].includes(phase()));
  const preparing = computed(() =>
    ["authenticating", "checking", "launching"].includes(preparation()),
  );

  const series = (id: string): ServerSeries => {
    let entry = serverSeries.get(id);
    if (!entry)
      serverSeries.set(id, (entry = { buckets: [], version: signal(0), summaries: signal({}) }));
    return entry;
  };

  function reset(): void {
    batch(() => {
      throughput.length = 0;
      throughputVersion.set(throughputVersion.peek() + 1);
      serverSeries.clear();
      seriesReset.set(seriesReset.peek() + 1);
      phase.set("idle");
      stage.set(null);
      step.set(null);
      shownAt.set(0);
      progress.set({ elapsedMs: 0, budgetMs: 0 });
      stall.set(null);
      live.set(null);
      stageBytes.set(0);
      results.set(NO_RESULTS);
      statuses.set({});
      details.set(null);
      result.set(null);
      error.set(null);
      plan.set(null);
      startError.set("");
      refusal.set("");
      prepared.set([]);
      preparation.set("idle");
    });
    stageBase = { stage: "", bytes: 0, last: 0 };
  }

  /** Lets go of the run: its late events can never reach the facts of a newer one. */
  function releaseRun(): void {
    release();
    release = () => {};
    run?.abort();
    run = null;
  }

  function follow(current: Run): void {
    const unsubscribe = current.on((event) => {
      switch (event.type) {
        case "phase":
          return batch(() => {
            if (event.phase !== phase.peek()) stageBytes.set(0);
            // A stage's warmup hands its live rate on to its window; only a new stage starts without one.
            if (event.stage !== stage.peek()) live.set(null);
            phase.set(event.phase);
            stage.set(event.stage);
            step.set(event.step);
            shownAt.set(event.t);
            if (event.step === "preparing") progress.set({ elapsedMs: 0, budgetMs: 0 });
            if (event.phase === "connecting") preparation.set("idle");
          });
        case "progress":
          return progress.set({ elapsedMs: event.elapsedMs, budgetMs: event.budgetMs });
        case "stall":
          return stall.set(event.stall);
        case "live": {
          const { sample } = event;
          if (sample.warmup) return live.set(sample);
          // A new stage counts from the last reading; a count that went backwards starts over.
          if (sample.stage !== stageBase.stage || sample.bytes < stageBase.last)
            stageBase = {
              stage: sample.stage,
              bytes: sample.bytes < stageBase.last ? 0 : stageBase.last,
              last: 0,
            };
          stageBase.last = sample.bytes;
          for (const dir of ["down", "up"] as const) {
            const bytesPerSec = sample[dir];
            if (bytesPerSec !== null)
              appendThroughput(
                throughput,
                {
                  t: sample.t,
                  bytesPerSec,
                  bytesCumulative: sample.bytes,
                  dir,
                  stage: sample.stage,
                  continuityId: sample.continuityId,
                },
                spanMs,
              );
          }
          return batch(() => {
            live.set(sample);
            stageBytes.set(sample.bytes - stageBase.bytes);
            throughputVersion.set(throughputVersion.peek() + 1);
          });
        }
        case "latency": {
          const entry = series(event.serverId);
          upsertLatency(entry.buckets, event.bucket);
          return entry.version.set(entry.version.peek() + 1);
        }
        case "summary": {
          const entry = series(event.serverId);
          return entry.summaries.set({ ...entry.summaries.peek(), [event.stage]: event.summary });
        }
        case "failure":
          env.recheck(event.failure);
          return details.set(current.details());
        case "stageEnd":
          return batch(() => {
            statuses.set({ ...statuses.peek(), [event.stage]: event.status });
            results.set(event.results);
            details.set(current.details());
          });
        case "complete": {
          const { result: finished, plans } = event;
          batch(() => {
            live.set(null);
            stall.set(null);
            result.set(finished);
            statuses.set(finished.stages);
            results.set(finished);
            details.set(finished.multiServer);
            stage.set(null);
            step.set(null);
            phase.set("complete");
          });
          env.finished({ result: finished, plans });
          const moved = STAGES.some(
            (each) => each !== "latency" && finished.stages[each] !== "not-run",
          );
          for (const { server, latency } of prepared.peek()) {
            const failed = (role: Role) =>
              finished.multiServer.failures.some(
                (failure) => failure.serverId === server.id && failure.scope === role,
              );
            env.proved(
              server.id,
              ROLES.filter((role) => (role === "throughput" ? moved : latency) && !failed(role)),
            );
          }
          return releaseRun();
        }
        case "error":
          env.recheck(null);
          batch(() => {
            live.set(null);
            stall.set(null);
            error.set({ reason: event.reason, message: event.message });
            phase.set("error");
          });
          return releaseRun();
        case "aborted":
          batch(() => {
            live.set(null);
            stall.set(null);
            phase.set("aborted");
          });
          // Nothing reads a run that ended; what it measured lives in the facts above.
          return releaseRun();
      }
    });
    release = unsubscribe;
  }

  async function launch(config: TestConfig, signal: AbortSignal): Promise<void> {
    await env.authorize(
      totalMs(adaptWarmup(config, LONGEST_WARMUP_RTT_MS)) + SESSION_MARGIN_MS,
      signal,
    );
    signal.throwIfAborted();
    preparation.set("checking");
    const ready = await env.prepare(signal, config);
    let next: Run;
    let focus: PreparedServer;
    let runPlan: RunPlan;
    try {
      signal.throwIfAborted();
      if (!ready.servers.length)
        throw new Error(
          ready.dropped
            .map(({ server, message }) =>
              ready.dropped.length > 1 ? `${server.name}: ${message}` : message,
            )
            .join("; ") || CHECK_FAILED,
        );
      focus = ready.servers.find(({ server }) => server.id === ready.focus) ?? ready.servers[0]!;
      runPlan = adaptWarmup(config, focus.latency?.rttMs ?? 0);
      next = new Run({
        servers: ready.servers,
        focus: focus.server.id,
        dropped: ready.dropped,
        plan: runPlan,
        open: env.open,
        mint: env.mint,
        streams: env.streams,
        clock: env.clock,
      });
    } catch (error) {
      ready.release();
      throw error;
    }
    // The previous result clears only now: canceling earlier keeps it. One batch, so Start never looks idle.
    batch(() => {
      releaseRun();
      env.finished(null);
      reset();
      preparation.set("launching");
      prepared.set(ready.servers);
      plan.set(runPlan);
      details.set(next.details());
    });
    run = next;
    rttMs = focus.latency?.rttMs ?? 0;
    spanMs = totalMs(runPlan);
    follow(next);
    const unfollow = release;
    release = () => {
      unfollow();
      ready.release();
    };
    next.start();
  }

  function toggle(): void {
    if (running.peek()) {
      cancel();
      run?.abort();
      return;
    }
    if (pending) return cancel();
    const refused = env.refusal.peek();
    if (refused) {
      refusedOutright = true;
      return batch(() => {
        startError.set(refused);
        preparation.set("blocked");
      });
    }
    if (env.blocker.peek()) return;
    const task = new AbortController();
    pending = task;
    batch(() => {
      startError.set("");
      preparation.set("authenticating");
    });
    const current = () => pending === task;
    launch(structuredClone(preferences.config.peek()), task.signal)
      .catch((cause) => {
        if (!current() || (cause instanceof DOMException && cause.name === "AbortError")) return;
        batch(() => {
          startError.set(cause instanceof Error ? cause.message : CHECK_FAILED);
          preparation.set(preparation.peek() === "authenticating" ? "blocked" : "failed");
        });
      })
      .finally(() => {
        if (!current()) return;
        pending = null;
        if (preparing.peek()) preparation.set("idle");
      });
  }

  function cancel(): void {
    pending?.abort();
    pending = null;
    refusedOutright = false;
    batch(() => {
      startError.set("");
      preparation.set("idle");
    });
  }

  const stopLapse = effect(() => {
    if (env.refusal() || !refusedOutright) return;
    untrack(cancel);
  });

  /** Whether a stage may be turned on or off now; while a test runs, the run's own rule decides. */
  const canToggle = (name: Stage) => {
    if (preparing()) return false;
    if (!running()) return true;
    stage();
    return run?.canChange(name) ?? false;
  };

  /** The one writer of test settings; while a run is live only stages, durations, early finish and the gauge may change. */
  function configure(patch: Partial<TestConfig>): boolean {
    const keys = Object.keys(patch);
    // A refusal speaks for the change that caused it, never for a later one.
    refusal.set("");
    // The gauge's maximum is the page's alone, so it may change even while a start prepares.
    if (preparing.peek() && keys.some((key) => key !== "visualization")) return false;
    const current = run && running.peek() ? run : null;
    if (
      current &&
      keys.some((key) => !["stages", "duration", "adaptive", "visualization"].includes(key))
    )
      return false;
    const candidate = { ...preferences.config.peek(), ...patch };
    if (
      !STAGES.some((name) => planned(candidate, name)) ||
      Object.values(candidate.duration).some((ms) => !Number.isFinite(ms) || ms < 0)
    )
      return false;
    if (current && keys.some((key) => key !== "visualization")) {
      const runPlan = adaptWarmup(
        {
          ...current.plan,
          stages: candidate.stages,
          duration: candidate.duration,
          adaptive: candidate.adaptive,
        },
        rttMs,
      );
      const refuse = (message: string) => (refusal.set(message), false);
      const remainingMs = totalMs(runPlan) - shownAt.peek() - progress.peek().elapsedMs;
      if (totalMs(runPlan) > totalMs(current.plan) && !env.fits(remainingMs + SESSION_MARGIN_MS))
        return refuse("This change would extend the test beyond the current sign-in.");
      try {
        current.reconfigure({
          stages: runPlan.stages,
          duration: runPlan.duration,
          adaptive: runPlan.adaptive,
        });
      } catch (cause) {
        return refuse(
          cause instanceof Error ? cause.message : "This change cannot apply to the running test.",
        );
      }
      plan.set(current.plan);
      spanMs = totalMs(current.plan);
    }
    // A change to what Start would run lapses a refused or failed start; the gauge alone leaves it be.
    if (keys.some((key) => key !== "visualization")) cancel();
    preferences.set({ config: candidate });
    return true;
  }

  function toggleStage(name: Stage): boolean {
    const config = preferences.config.peek();
    const stages = { ...config.stages, [name]: !config.stages[name] };
    if (!canToggle(name) || !STAGES.some((each) => stages[each])) return false;
    return configure({ stages });
  }

  function returnToStart(): void {
    cancel();
    releaseRun();
    env.finished(null);
    reset();
  }

  function end(reason: FailureReason, message: string): void {
    if (running.peek()) run?.end(reason, message);
  }

  return {
    phase: phase as Read<ShownPhase>,
    stage: stage as Read<Stage | null>,
    step: step as Read<Step | null>,
    shownAt: shownAt as Read<number>,
    progress: progress as Read<{ elapsedMs: number; budgetMs: number }>,
    stall: stall as Read<Stall | null>,
    live: live as Read<LiveSample | null>,
    stageBytes: stageBytes as Read<number>,
    throughput: throughput as readonly ThroughputSample[],
    throughputVersion: throughputVersion as Read<number>,
    /** One server's latency buckets and summaries; read `version` to follow the buckets. */
    latency: (id: string): LatencySeries => {
      seriesReset();
      return series(id);
    },
    results: results as Read<StageResults>,
    statuses: statuses as Read<Partial<Record<Stage, StageStatus>>>,
    details: details as Read<MultiServerResult | null>,
    result: result as Read<RunResult | null>,
    error: error as Read<{ reason: FailureReason; message: string } | null>,
    plan: plan as Read<RunPlan | null>,
    preparation: preparation as Read<Preparation>,
    startError: startError as Read<string>,
    refusal: refusal as Read<string>,
    prepared: prepared as Read<readonly PreparedServer[]>,
    running,
    preparing,
    canToggle,
    toggle,
    cancel,
    configure,
    toggleStage,
    returnToStart,
    end,
    dispose() {
      stopLapse();
      cancel();
      releaseRun();
    },
  };
}
