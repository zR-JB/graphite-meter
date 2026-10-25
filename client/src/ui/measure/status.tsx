// What the run says outside the instrument: the status strip, the toast and the connection dot.
import type { App } from "../../app/app";
import type { LatencyBucket } from "../../engine/measure/buckets";
import { needsPings } from "../../engine/net/streams";
import { type Cause, movedBytes } from "../../engine/run/result";
import { activities } from "../../engine/run/schedule";
import { Show } from "../../lib/dom";
import { handoff } from "../../lib/motion";
import { computed, effect, onCleanup, signal, untrack } from "../../lib/signal";
import { fmtBytes, fmtCounted, fmtDuration, fmtMs } from "../../view/format";
import {
  CONNECTIVITY,
  OUTCOME,
  PHASE,
  reasonLabel,
  STAGE_STATE,
  statusLabel,
  type Connectivity,
  type Tone,
} from "../../view/words";
import { announceChanges } from "../kit/announce";
import { Icon } from "../kit/icon";
import { tip } from "../kit/tip";
import type { Shell } from "../shell/api";
import { causePlace } from "./cards-model";
import { TOAST } from "./copy";
import { liveOf } from "./live";
import { Figure, painter, setText } from "./paint";

/** The word cell is as wide as the widest word it can say, so the figures stay where they are. */
const WORDS = [
  ...new Set([
    ...Object.values(PHASE),
    ...Object.values(OUTCOME).map((outcome) => outcome.label),
    STAGE_STATE.failed.label,
  ]),
];

/** The status strip; each figure's cell is as wide as its longest value, so counting never moves its neighbors. */
export function StatusBar(props: { app: App; shell: Shell }): Node {
  const { app } = props;
  const { run } = app;
  const live = liveOf(app);
  const label = handoff(
    () => statusLabel(live.preparation(), run.phase(), run.result()),
    (word) => word,
  );
  const refused = computed(
    () => live.preparation() === "blocked" || live.preparation() === "failed",
  );
  const bytes = computed(() => {
    const result = run.result();
    if (result) return movedBytes(result.multiServer);
    run.throughputVersion();
    return run.throughput.at(-1)?.bytesCumulative ?? 0;
  });
  const recovering = computed(() => run.running() && run.stall() !== null);
  const left = handoff(
    () => ({ show: run.running() && live.budget() > 0, recovering: recovering() }),
    (view) => `${view.show}:${view.recovering}`,
  );

  const elapsed = new Figure("readout", 7);
  const remaining = new Figure("readout", 7);
  const moved = new Figure("readout", 7);
  const recoveringText = document.createTextNode("");
  effect(() => moved.set(fmtBytes(bytes(), app.preferences.unitBase())));
  const wake = painter(
    (now) => {
      // A stopped run keeps the time it showed when it stopped.
      const phase = run.phase.peek();
      if (phase !== "aborted")
        elapsed.set(fmtDuration(run.result.peek()?.durationMs ?? live.runClock.at(now)));
      const rest = fmtDuration(Math.max(0, live.budget.peek() - live.phaseClock.at(now)));
      if (left.shown.peek().recovering) setText(recoveringText, rest);
      else remaining.set(rest);
      return live.active(now);
    },
    undefined,
    live.beat,
  );
  effect(() => {
    live.moved();
    run.result();
    left.shown();
    wake();
  });

  return (
    <>
      <span class="label-cell">
        {WORDS.map((word) => (
          <span class="sizer" aria-hidden="true">
            {word}
          </span>
        ))}
        <span
          class={() => (label.out() ? "label handoff handoff-out" : "label handoff")}
          ref={tip(() => (refused() ? run.startError() || app.blocker() : ""))}
        >
          {label.shown}
        </span>
      </span>
      <span class={() => (left.shown().show ? "elapsed secondary" : "elapsed")}>{elapsed.box}</span>
      <span class="transferred">{moved.box}</span>
      <Show when={() => left.shown().show}>
        {() => (
          <span
            class={() => (left.out() ? "remaining handoff handoff-out" : "remaining handoff")}
            data-tone={() => (left.shown().recovering ? CONNECTIVITY.recovering.tone : undefined)}
          >
            {() =>
              left.shown().recovering
                ? [`${CONNECTIVITY.recovering.label}, `, recoveringText, " left"]
                : [remaining.box, " left"]
            }
          </span>
        )}
      </Show>
      <span class="build">{`${BUILD.profile} ${BUILD.clientVersion}`}</span>
    </>
  );
}

/** A toast lingers for its reading time, then goes; not motion. */
const LINGER_MS = 3_200;
const LEAVE_MS = 150;

/** Only failures, issues, stalls and a result History could not save; a stall shows while it lasts, an unsaved warning while unsaved. */
export function PhaseToast(props: { app: App; shell: Shell }): Node {
  const { app } = props;
  const { run } = app;
  const stalled = computed(() => run.running() && run.stall() !== null);
  const stall = computed(() => ({
    kicker: CONNECTIVITY.recovering.label,
    message: reasonLabel(run.stall()?.reason ?? "connection-lost"),
    tone: CONNECTIVITY.recovering.tone as Tone,
  }));
  const causes = computed(() => run.table()?.causes ?? []);
  const keyOf = (cause: Cause) => `${cause.serverId} ${cause.stage} ${cause.scope} ${cause.reason}`;
  const named = (cause: Cause) => {
    const server = run.table()!.rows.find((row) => row.server.id === cause.serverId)!.server.name;
    const several = (run.details()?.selection.length ?? 0) > 1;
    // Where it happened leads, as the card it marks names its stage: "Bergen · Start", then what happened.
    return {
      kicker: several ? `${server} · ${causePlace(cause)}` : causePlace(cause),
      message: reasonLabel(cause.reason),
    };
  };
  const toast = signal<{ kicker: string; message: string; tone: Tone }>({
    kicker: "",
    message: "",
    tone: "err",
  });
  const lingering = signal(false);
  let timer = 0;
  onCleanup(() => clearTimeout(timer));
  const show = (next: { kicker: string; message: string; tone: Tone } | null) => {
    clearTimeout(timer);
    lingering.set(!!next);
    if (!next) return;
    toast.set(next);
    timer = window.setTimeout(() => lingering.set(false), LINGER_MS);
  };
  // A toast notes each cause as it arrives, the newest when several do.
  let seen = new Set<string>();
  effect(() => {
    const phase = run.phase();
    const all = causes();
    if (phase === "idle") {
      seen = new Set();
      return show(null);
    }
    const fresh = all.filter((cause) => !seen.has(keyOf(cause)));
    seen = new Set(all.map(keyOf));
    const newest = fresh.reduce<Cause | null>(
      (latest, each) => (!latest || each.atMs >= latest.atMs ? each : latest),
      null,
    );
    if (newest) show({ ...untrack(() => named(newest)), tone: "err" });
  });
  const unsaved = computed(
    () =>
      !run.running() &&
      !!run.result() &&
      app.saving() &&
      app.saver.unsaved() &&
      !!app.saver.warning(),
  );
  const shown = computed(() =>
    stalled()
      ? stall()
      : unsaved() && !lingering()
        ? { kicker: TOAST.unsaved, message: app.saver.warning(), tone: "warn" as Tone }
        : toast(),
  );
  announceChanges(() => (stalled() ? `${stall().kicker}: ${stall().message}` : ""));
  announceChanges(() => (unsaved() ? `${TOAST.unsaved}: ${app.saver.warning()}` : ""));
  announceChanges(() => (lingering() ? `${toast().kicker}: ${toast().message}` : ""));
  // Out of layout while it shows nothing, so a stage that resizes never lays it out; laid out for its fade as it goes.
  const showing = computed(() => lingering() || stalled() || unsaved());
  const leaving = signal(false);
  let shownBefore = false;
  let leaveTimer = 0;
  onCleanup(() => clearTimeout(leaveTimer));
  effect(() => {
    const now = showing();
    untrack(() => {
      clearTimeout(leaveTimer);
      if (now || !shownBefore) {
        shownBefore = now;
        return leaving.set(false);
      }
      shownBefore = false;
      leaving.set(true);
      leaveTimer = window.setTimeout(() => leaving.set(false), LEAVE_MS);
    });
  });
  const element = (
    <div
      class={() =>
        `float phase-toast${showing() ? " visible" : leaving() ? " leaving" : ""}${stalled() ? " stall" : ""}`
      }
      data-tone={() => shown().tone}
      aria-hidden="true"
    >
      <span class="notice-icon">
        <Icon name="info" />
      </span>
      <span class="kicker">{() => shown().kicker}</span>
      <strong>{() => shown().message}</strong>
    </div>
  ) as HTMLElement;
  element.style.setProperty("--linger", `${LINGER_MS}ms`);
  element.style.setProperty("--leave", `${LEAVE_MS}ms`);
  return element;
}

/** Evidence older than this at the run's own clock is stale. */
const STALE_MS = 3_000;
const WINDOW_MS = 4_000;

/** The probes of the last WINDOW_MS of evidence: what the dot judges and states. */
function recentProbes(buckets: readonly LatencyBucket[]) {
  const latest = buckets.at(-1);
  const recent = latest
    ? buckets.filter((bucket) => bucket.endT >= latest.endT - WINDOW_MS && bucket.pingCount > 0)
    : [];
  const replies = recent
    .flatMap((bucket) => (bucket.medianRttMs === null ? [] : [bucket.medianRttMs]))
    .sort((a, b) => a - b);
  return {
    recent,
    replies,
    probes: recent.reduce((sum, bucket) => sum + bucket.pingCount, 0),
    timeouts: recent.reduce((sum, bucket) => sum + bucket.timeoutCount, 0),
  };
}

/** A live verdict from missing replies only, never from latency, which a loaded stage raises by design. */
function connectionQuality(
  buckets: readonly LatencyBucket[],
  nowT = Number.NEGATIVE_INFINITY,
): Connectivity {
  const latest = buckets.at(-1);
  if (!latest || nowT - latest.endT > STALE_MS) return "checking";
  const { recent, probes, timeouts } = recentProbes(buckets);
  // A clean tail of replies and elapsed time supersedes old timeouts at any cadence.
  let clean = 0;
  for (let i = recent.length - 1; i >= 0; i--) {
    const bucket = recent[i]!;
    if (bucket.timeoutCount || bucket.medianRttMs === null) break;
    clean += bucket.pingCount;
    if (clean >= 2 && i < recent.length - 1 && latest.endT - bucket.startT >= 800)
      return "connected";
  }
  // One timeout is too little evidence, above all at the sparse idle cadence.
  if (timeouts >= 2 && timeouts / probes >= 0.2) return "unstable";
  if (timeouts >= 2 && timeouts / probes >= 0.02) return "degraded";
  return "connected";
}

/** The connection as a dot in its tone, its facts in a tip; only settled trouble is spoken. */
export function ConnectivityIndicator(props: { app: App; shell: Shell }): Node {
  const { app } = props;
  const { run, servers } = app;
  const live = liveOf(app);
  const state = computed<Connectivity>(() => {
    const running = run.running();
    const selection = servers.selectionState();
    if (!running) {
      if (
        run.preparing() ||
        servers.loading() ||
        selection === "checking" ||
        selection === "unchecked"
      )
        return "checking";
      if (selection === "failed")
        return servers.selected().some((id) => servers.views().get(id)?.readiness === "verified")
          ? "degraded"
          : "offline";
      if (
        servers.views().get(servers.latencyServer() ?? "")?.connectivity === "offline" &&
        app.idle.buckets.length
      )
        return "offline";
      app.idle.version();
      const idle = connectionQuality(app.idle.buckets);
      return idle === "checking" ? "connected" : idle;
    }
    if (run.stall()) return "recovering";
    const stage = run.stage();
    const plan = run.plan();
    const activity =
      stage && plan ? activities(plan).find((each) => each.stage === stage) : undefined;
    return activity && needsPings(activity)
      ? connectionQuality(live.buckets(), run.shownAt() + run.progress().elapsedMs)
      : "connected";
  });
  const word = computed(() => CONNECTIVITY[state()]);
  const label = computed(() => `Connection: ${word().label}`);
  // The values behind the state, so the dot is never a verdict alone.
  const facts = computed(() => {
    const { replies, probes, timeouts } = recentProbes(live.pulse());
    const selected = servers.selected();
    const median = replies.length
      ? replies[(replies.length - 1) >> 1]! / 2 + replies[replies.length >> 1]! / 2
      : null;
    return [
      state() === "recovering" && run.stall() ? reasonLabel(run.stall()!.reason) : "",
      !run.running() && servers.selectionState() === "failed"
        ? `${selected.filter((id) => servers.views().get(id)?.readiness === "verified").length} of ${fmtCounted(selected.length, "server")} ready`
        : "",
      median === null
        ? ""
        : `Median ${fmtMs(median)} ms, last ${fmtDuration(WINDOW_MS, 0)} of probes`,
      probes ? `${timeouts} of ${fmtCounted(probes, "probe")} timed out` : "No probes yet",
    ].filter(Boolean);
  });
  // Checking flaps every run and the toast speaks a stall.
  announceChanges(() => (["degraded", "unstable", "offline"].includes(state()) ? label() : ""));
  return (
    <div class="pulse" ref={tip(() => [label(), ...facts()].join("\n"))}>
      <span class="sr-only">
        <span class="spoken">{() => [label(), ...facts()].join(". ")}</span>
      </span>
      <span class="status-dot" data-tone={() => word().tone ?? undefined} />
    </div>
  );
}
