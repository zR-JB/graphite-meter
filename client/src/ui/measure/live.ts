// Holds the instrument's live values, one set per app, which say how the run model's facts move on screen.
import type { App } from "../../app/app";
import type { Preparation, ShownPhase } from "../../app/run";
import type { LatencyBucket } from "../../engine/measure/buckets";
import type { ServerIdentity } from "../../engine/types";
import type { LatencySummary } from "../../engine/measure/latency";
import { needsPings } from "../../engine/net/streams";
import type { Phase } from "../../engine/run/run";
import {
  activities,
  planned,
  STAGES,
  type Stage,
  type TransferStage,
} from "../../engine/run/schedule";
import { nextFrame, pageMs } from "../../lib/frame";
import { Glide } from "../../lib/motion";
import { batch, computed, effect, scope, signal, untrack, type Read } from "../../lib/signal";
import type { RateUnits, Units } from "../../view/format";
import { latencyAxisMs, throughputScales } from "../../view/scales";
import { stageState, type StageFacts, type StageState } from "../../view/words";
import { resultArcs } from "./dial-model";

/** A stalled sample fades the rates to zero once, over this long. */
const STALL_FADE_MS = 800;
/** Views keep painting this long after the last change, for glides to land. */
const SETTLE_MS = 1_000;
const RUNNING: readonly ShownPhase[] = ["connecting", "warmup", ...STAGES];
const runningPhase = (phase: ShownPhase): phase is Phase => RUNNING.includes(phase);

export type Live = ReturnType<typeof createLive>;

const lives = new WeakMap<App, Live>();

export function liveOf(app: App): Live {
  let live = lives.get(app);
  if (!live) {
    [live] = scope(() => createLive(app));
    lives.set(app, live);
  }
  return live;
}

function createLive(app: App) {
  const { run, preferences, servers } = app;
  const units = computed<Units>(() => ({
    base: preferences.unitBase(),
    kind: preferences.unitKind(),
  }));
  let movedAt = Number.NEGATIVE_INFINITY;
  const moved = signal(0);
  const bump = () => {
    movedAt = pageMs();
    moved.set(moved.peek() + 1);
  };

  // The peaks are running maxima of what live ticks drew, warmups included, so axes never step back; a new run starts them over.
  const peaks = signal({ lane: 0, combined: 0 });
  effect(() => {
    run.throughputVersion();
    const sample = run.live();
    untrack(() => {
      const current = peaks.peek();
      if (!run.throughput.length && !sample) {
        if (current.lane || current.combined) peaks.set({ lane: 0, combined: 0 });
        return;
      }
      const down = sample?.down ?? 0;
      const up = sample?.up ?? 0;
      if (Math.max(down, up) > current.lane || down + up > current.combined)
        peaks.set({
          lane: Math.max(current.lane, down, up),
          combined: Math.max(current.combined, down + up),
        });
    });
  });
  const scales = computed(() =>
    throughputScales(
      run.results(),
      { ...peaks(), running: run.running() },
      preferences.config().visualization.throughputMaxBytesPerSec,
      units(),
    ),
  );
  /** Every rate on the page reads in one unit, the dial's. */
  const rateUnits = computed<RateUnits>(() => ({ ...units(), tier: scales().tier }));

  // The rates show one stage at a time; a new stage has no rate, its first evidence snaps in and a stall fades once.
  const down = new Glide();
  const up = new Glide();
  const rateStage = signal<TransferStage | null>(null);
  let stalled = false;
  effect(() => {
    const sample = run.live();
    // A stopped run keeps the rate it last showed, until the next run leaves "aborted".
    const held = !sample && run.phase() === "aborted";
    untrack(() => {
      if (!held && sample?.stage !== rateStage.peek()) {
        rateStage.set(null);
        stalled = false;
      }
      if (!sample || (sample.down === null && sample.up === null) || (sample.stalled && stalled))
        return;
      const correction = {
        snap: rateStage.peek() === null,
        over: sample.stalled ? STALL_FADE_MS : undefined,
      };
      stalled = sample.stalled;
      down.set(sample.down ?? 0, correction);
      up.set(sample.bridgedUp ?? sample.up ?? 0, correction);
      rateStage.set(sample.stage);
      bump();
    });
  });

  // The result lens and the latency focus; all servers follow the run's focus, and a server shown alone takes it when it measured latency.
  const lens = signal("");
  const chosen = signal<string | null>(null);
  const measuredLatency = (id: string | null) =>
    !!id &&
    (run.details.peek()?.servers.some((entry) => entry.server.id === id && entry.latencyTarget) ??
      true);
  function showServer(id: string): void {
    const pin = app.latencyPin.peek();
    batch(() => {
      lens.set(id);
      chosen.set(
        measuredLatency(id)
          ? id
          : measuredLatency(pin)
            ? pin
            : (run.details.peek()?.latencyFocus ?? null),
      );
    });
  }
  /** Shows one server's latency, mid-run included; every server is probed alike, only the view changes. */
  function showLatency(id: string): void {
    batch(() => {
      app.latencyPin.set(id);
      chosen.set(id);
    });
  }
  effect(() => {
    const details = run.details();
    const running = run.running();
    untrack(() => {
      if (!details)
        return batch(() => {
          lens.set("");
          chosen.set(null);
        });
      if (running) lens.set("");
      if (!lens.peek()) showServer("");
    });
  });
  const focus = computed(() => chosen() ?? servers.latencyServer());
  const series = computed(() => {
    const id = focus();
    return id ? run.latency(id) : null;
  });
  // A late reply revises a closed bucket hundreds of times a second, so views follow the series at most once a frame.
  const bucketFrame = signal(0);
  let bucketPending: (() => void) | null = null;
  effect(() => {
    series()?.version();
    untrack(() => {
      if (!bucketPending)
        bucketPending = nextFrame(() => {
          bucketPending = null;
          bucketFrame.set(bucketFrame.peek() + 1);
        });
    });
  });
  const buckets = (): readonly LatencyBucket[] => {
    bucketFrame();
    return series()?.buckets ?? [];
  };
  /** Gives the idle monitor's probes between runs, else the run's. */
  const pulse = (): readonly LatencyBucket[] => {
    app.idle.version();
    const own = buckets();
    return run.running() || !app.idle.buckets.length ? own : app.idle.buckets;
  };
  const replies = computed(() => {
    const newest = pulse().at(-1);
    // Until the window's first bucket closes, the latency warmup's own replies read.
    const warm = !newest && run.stage() === "latency" ? run.warmupRtt()[focus() ?? ""] : undefined;
    const view = servers.views().get(servers.latencyServer() ?? "");
    return {
      rttMs: newest?.medianRttMs ?? warm ?? view?.latency.path?.rttMs ?? 0,
      lost: !!newest && newest.pingCount > 0 && newest.medianRttMs === null,
    };
  });
  /** The dial reads the finished run's own latency when it measured no transfer. */
  const completedRtt = computed(() => {
    const result = run.phase() === "complete" ? run.result() : null;
    return result && !resultArcs(result).length ? (result.latency?.reportedMs ?? null) : null;
  });
  /** The dial reads milliseconds through the latency stage, its warmup included, and after a latency-only run. */
  const msScale = computed(() => {
    const phase = run.phase();
    return (
      phase === "latency" ||
      (phase === "warmup" && run.stage() === "latency") ||
      completedRtt() !== null
    );
  });
  const latencyAxis = computed(() => latencyAxisMs(buckets(), !run.running()));
  // The round trip shows only on the millisecond scale; between runs the idle probes move it unseen and wake nothing.
  const rtt = new Glide();
  effect(() => {
    const ms = completedRtt() ?? replies().rttMs;
    const shown = msScale();
    untrack(() => {
      rtt.set(ms, { snap: !shown });
      if (shown) bump();
    });
  });

  /** Each transfer stage's sample count and first and last time, as a key that changes only with that stage's samples. */
  const stageSamples = computed(() => {
    run.throughputVersion();
    const seen: Partial<Record<TransferStage, { count: number; first: number; last: number }>> = {};
    for (const sample of run.throughput) {
      const entry = (seen[sample.stage] ??= { count: 0, first: sample.t, last: sample.t });
      entry.count++;
      entry.last = sample.t;
    }
    return seen;
  });
  /** Keys each bucket phase so the key changes only when that phase's buckets do. */
  const bucketKeys = computed(() => {
    const keys: Partial<Record<LatencyBucket["phase"], number>> = {};
    for (const bucket of buckets())
      keys[bucket.phase] =
        (keys[bucket.phase] ?? 0) +
        bucket.endT +
        (bucket.medianRttMs ?? -1) +
        (bucket.minRttMs ?? 0) * 3 +
        (bucket.maxRttMs ?? 0) * 7;
    return keys;
  });

  /** Gives the streamed summaries while running, the saved ones once complete. */
  const summaries = computed<Partial<Record<Stage, LatencySummary | null>>>(() => {
    const id = focus();
    const saved = run.details()?.servers.find((entry) => entry.server.id === id)?.latencyByStage;
    const streamed = id ? run.latency(id).summaries() : {};
    if (run.phase() === "complete" && saved) return saved;
    return Object.keys(streamed).length ? streamed : (saved ?? {});
  });

  // Progress corrects the shown segment's clock, a running segment keeps it moving, a new segment starts it at zero.
  const phaseClock = new Glide();
  const budget = signal(0);
  let segment = "";
  let stale: object | null = null;
  let fresh = true;
  effect(() => {
    const progress = run.progress();
    const key = `${run.phase()}:${run.shownAt()}`;
    const step = run.step();
    untrack(() => {
      if (run.phase.peek() === "aborted") phaseClock.hold();
      else if (key !== segment) {
        segment = key;
        stale = progress;
        fresh = true;
        phaseClock.set(0, { snap: true });
        budget.set(0);
      } else if (progress !== stale) {
        const running = step === "running" && progress.budgetMs > 0;
        phaseClock.set(progress.elapsedMs, {
          rate: running ? 1 : 0,
          max: progress.budgetMs,
          snap: fresh,
        });
        budget.set(progress.budgetMs);
        fresh = false;
      }
      bump();
    });
  });

  // The run clock counts from the start; it holds when the run stops and reads the result's own duration once complete.
  const runClock = new Glide();
  effect(() => {
    const phase = run.phase();
    const result = run.result();
    untrack(() => {
      if (phase === "connecting") runClock.set(0, { rate: 1, snap: true });
      else if (phase === "idle") runClock.set(0, { snap: true });
      else if (phase === "complete" && result) runClock.set(result.durationMs, { snap: true });
      else if (phase === "aborted") runClock.hold();
      bump();
    });
  });

  // Each stage's state in the shown run, from one projection of its facts; an ended run keeps its last phase for its stage.
  const lastPhase = signal<Phase | null>(null);
  effect(() => {
    const phase = run.phase();
    untrack(() => {
      if (runningPhase(phase)) lastPhase.set(phase);
      else if (phase === "idle") lastPhase.set(null);
    });
  });
  const shownPlan = computed(() => run.plan() ?? preferences.config());
  const states = computed<Record<Stage, StageState>>(() => {
    const phase = run.phase();
    const shown = runningPhase(phase) ? phase : lastPhase();
    const current = run.stage();
    const statuses = run.statuses();
    const stall = run.stall() !== null;
    const ended: StageFacts["ended"] =
      phase === "complete" ? "complete" : phase === "aborted" ? "stopped" : null;
    const plan = shownPlan();
    const entries = STAGES.map((stage) => {
      const mine = current === stage;
      const facts = {
        planned: planned(plan, stage),
        settled: statuses[stage] ?? null,
        phase: mine ? shown : null,
        stalled: mine && stall,
        ended,
      };
      return [stage, stageState(facts)] as const;
    });
    return Object.fromEntries(entries) as Record<Stage, StageState>;
  });

  /** Says which surfaces show a stage, one rule for each, side by side. */
  const visibility = (stage: Stage) => {
    const next = planned(preferences.config(), stage);
    const shown = states()[stage] !== "skipped";
    return {
      card: next || shown,
      chip: stage !== "bidirectional" || next || shown,
      lane: planned(shownPlan(), stage),
    };
  };
  /** A start the servers already refuse reads as blocked before anyone presses Start. */
  const preparation = computed<Preparation>(() => {
    const own = run.preparation();
    return own === "idle" && run.phase() === "idle" && !servers.loading() && app.blocker()
      ? "blocked"
      : own;
  });
  /** The servers the run indicates: its own selection, else the selected ones in catalog order, as discovery names them. */
  const indicated = computed<readonly ServerIdentity[]>(() => {
    const details = run.details();
    if (details) return details.selection;
    const views = servers.views();
    const selected = servers.selected();
    return (servers.catalog()?.servers ?? [])
      .filter((entry) => selected.includes(entry.id))
      .map((entry) => views.get(entry.id)?.server ?? entry);
  });
  /** The lanes need probes the next run sends. */
  const latencyEnabled = computed(() => activities(preferences.config()).some(needsPings));

  return {
    units,
    rateUnits,
    scales,
    down,
    up,
    /** The rates show this transfer stage's evidence; null before its first. */
    rateStage: rateStage as Read<TransferStage | null>,
    rtt,
    /** Gives the newest reply's median, unanimated, and whether the newest probes went unanswered. */
    replies,
    completedRtt,
    msScale,
    latencyAxis,
    phaseClock,
    /** Holds the shown segment's length once its progress arrives; 0 before. */
    budget: budget as Read<number>,
    runClock,
    moved: moved as Read<number>,
    beat: () => (rateStage.peek() === null ? null : moved.peek()),
    /** Painting may stop once no run is live and the last change has landed. */
    active: (now: number) => run.running.peek() || now - movedAt < SETTLE_MS,
    states,
    /** Keeps the phase the shown run was last in once it ends; a stop in a stage's warmup reached none of it. */
    endedIn: lastPhase as Read<Phase | null>,
    visibility,
    preparation,
    latencyEnabled,
    /** The cards show this server; "" for every server together. */
    lens: lens as Read<string>,
    indicated,
    showServer,
    showLatency,
    focus,
    buckets,
    bucketKeys,
    stageSamples,
    pulse,
    summaries,
  };
}
