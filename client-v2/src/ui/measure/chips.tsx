// The run key over the stage chips, as wide as their row. A chip switches its stage for the next run and shows how it
// fares in the shown one: a bar along its top, and at its end its time into the stage, its length, a check or a word.
import type { App } from "../../app/app";
import { adaptWarmup, planned, STAGES, timeline, type Stage } from "../../engine/run/schedule";
import { For, Show } from "../../lib/dom";
import type { Child } from "../../lib/jsx-runtime";
import { handoff } from "../../lib/motion";
import { computed, effect } from "../../lib/signal";
import { fmtDuration, fmtLatency, fmtProgress, fmtRate, fmtStageTime } from "../../view/format";
import { NEXT_RUN, PHASE, reasonLabel, STAGE, STAGE_STATE, type StageState, type Tone } from "../../view/words";
import { flip } from "../flip";
import { announceChanges } from "../kit/announce";
import { Icon } from "../kit/icon";
import { clip, tip } from "../kit/tip";
import { CHIPS, runAction } from "./copy";
import type { Live } from "./live";
import { Figure, painter } from "./paint";

/** How a chip looks: off, waiting, or the stage's state in the shown run. */
type Look = "disabled" | "pending" | "warmup" | "active" | "recovering" | "complete" | "partial" | "failed";
const LOOK: Record<StageState, Look> = {
  skipped: "pending",
  upcoming: "pending",
  unreached: "pending",
  stopped: "pending",
  warmup: "warmup",
  running: "active",
  recovering: "recovering",
  complete: "complete",
  partial: "partial",
  failed: "failed",
};
const RUNNING_LOOKS = new Set<Look>(["warmup", "active", "recovering"]);
const SETTLED_WORD: Partial<Record<Look, string>> = {
  complete: STAGE_STATE.complete.label,
  partial: STAGE_STATE.partial.label,
  failed: STAGE_STATE.failed.label,
};

export function StageTrack(props: { app: App; live: Live; children: Child }): Node {
  const { app, live } = props;
  const shown = computed(() => STAGES.filter((stage) => live.visibility(stage).chip));
  const track = (
    <div class="stage-track">
      {props.children}
      <div class="chips" role="group" aria-label={CHIPS.legend}>
        <span class="legend caps" aria-hidden="true">
          {CHIPS.legend}
        </span>
        <For each={shown} by={(stage) => stage}>
          {(stage) => <Chip app={app} live={live} stage={stage()} />}
        </For>
      </div>
      {/* A stage change the run refused says why where it was asked for. */}
      <Show when={app.run.refusal}>{(text) => <p class="track-refusal">{text}</p>}</Show>
    </div>
  ) as HTMLElement;
  announceChanges(app.run.refusal);
  const chips = track.querySelector<HTMLElement>(".chips")!;
  effect(() => {
    const count = shown().length;
    track.style.setProperty("--chips", String(count));
    track.style.setProperty("--cols", String(count > 3 ? 2 : count));
    chips.dataset.count = String(count);
  });
  return track;
}

function Chip(props: { app: App; live: Live; stage: Stage }): Node {
  const { app, live, stage } = props;
  const { run } = app;
  const config = app.preferences.config;
  const selected = computed(() => planned(config(), stage));
  const locked = computed(() => !run.canToggle(stage));
  const state = computed(() => live.states()[stage]);
  // A stage the next run includes but the shown run did not run waits for the next run.
  const nextRun = computed(() => selected() && state() === "skipped" && run.plan() !== null);
  const look = computed<Look>(() => (selected() ? LOOK[state()] : "disabled"));
  const tag = computed(() => {
    if (!selected()) return STAGE_STATE.skipped.label;
    if (nextRun()) return NEXT_RUN.label;
    return look() === "partial" || look() === "failed" ? STAGE_STATE[look() as "partial" | "failed"].label : null;
  });
  // Why a locked chip cannot change; a settled one says nothing more than its check or tag.
  const reason = computed(() => {
    if (tag() !== null) return tag();
    if (!locked() || look() === "complete") return null;
    if (run.stage() === stage && RUNNING_LOOKS.has(look()))
      return look() === "recovering" ? STAGE_STATE.recovering.label : STAGE_STATE.running.label;
    return STAGE_STATE.upcoming.label;
  });
  const timed = computed(() => look() === "active" && live.budget() > 0);
  // A waiting chip shows its stage's length: the shown run's plan, or the next run's for a stage queued for it.
  const waiting = computed(() => selected() && look() === "pending");
  const length = computed(() => fmtStageTime((nextRun() ? config() : (run.plan() ?? config())).duration[`${stage}Ms`]));
  const end = handoff(
    () => {
      const now = look();
      if (now === "complete") return { key: `${now}:check`, check: true, text: "", time: false, tone: null };
      if (timed()) return { key: `${now}:live`, check: false, text: "", time: true, tone: null };
      if (waiting()) return { key: `${now}:${length()}`, check: false, text: length(), time: true, tone: null };
      const tone: Tone | null = now === "partial" || now === "recovering" ? "warn" : now === "failed" ? "err" : null;
      return { key: `${now}:${reason() ?? ""}`, check: false, text: reason() ?? "", time: false, tone };
    },
    (view) => view.key,
  );

  const value = computed(() => {
    if (stage === "latency") {
      const latency = run.results().latency;
      return latency ? fmtLatency(latency.reportedMs) : "";
    }
    const results = run.results();
    const down = results.bidirectional?.down?.reportedBytesPerSec;
    const up = results.bidirectional?.up?.reportedBytesPerSec;
    const bytes =
      stage === "bidirectional" ? (down != null && up != null ? down + up : null) : results[stage]?.reportedBytesPerSec;
    return bytes == null ? "" : fmtRate(bytes, live.rateUnits());
  });
  const failure = computed(() => {
    const found = run
      .details()
      ?.failures.find((each) => each.stage === stage && (each.scope === "latency") === (stage === "latency"));
    return found ? reasonLabel(found.reason) : "";
  });
  // One line the chip does not already show: its result, or why it is locked.
  const tipText = computed(() => {
    const settled = SETTLED_WORD[look()];
    if (settled) return [settled, value(), failure()].filter(Boolean).join("\n");
    if (!locked()) return "";
    if (RUNNING_LOOKS.has(look())) return CHIPS.lockedRunning;
    const starting = run.preparing() || run.phase() === "connecting";
    return look() === "pending" && starting ? CHIPS.lockedStarting : CHIPS.lockedUntilEnd;
  });

  const fill = (<span class="chip-fill" />) as HTMLElement;
  const full = new Figure("full");
  const short = new Figure("short");
  effect(() => {
    const now = look();
    fill.dataset.tone = now === "recovering" ? "warn" : stage;
    fill.className = [
      "chip-fill",
      now === "warmup" && "chip-fill--warmup",
      now === "failed" && "chip-fill--failed",
      now === "partial" && "is-partial",
      now === "recovering" && "is-stalled",
      timed() && "is-live",
    ]
      .filter(Boolean)
      .join(" ");
  });
  // The bar: its stage's progress while it runs, the fraction a stopped stage reached, full once it settled. Only a
  // chip whose bar follows the clock draws every frame; the rest draw once per change.
  const clocked = computed(() => {
    const now = look();
    return now === "active" || now === "recovering" || (state() === "stopped" && live.endedIn() === stage);
  });
  let progress = Number.NaN;
  let tenths = Number.NaN;
  const draw = (now: number) => {
    const looks = look.peek();
    const length = live.budget.peek();
    const clock = live.phaseClock.at(now);
    const next =
      looks === "warmup" || looks === "failed" || looks === "complete" || looks === "partial"
        ? 1
        : clocked.peek() && length > 0
          ? Math.min(1, clock / length)
          : 0;
    if (!(Math.abs(next - progress) < 5e-4)) fill.style.setProperty("--progress", String((progress = next)));
    if (timed.peek() && Math.round(clock / 100) !== tenths) {
      tenths = Math.round(clock / 100);
      full.set(fmtProgress(clock, length));
      short.set(fmtDuration(clock, 1));
    }
    return clocked.peek() && live.active(now);
  };
  const wake = painter(draw);
  effect(() => {
    look();
    timed();
    if (clocked()) {
      live.budget();
      run.running();
    }
    tenths = Number.NaN;
    wake();
  });

  const label = STAGE[stage].short;
  return (
    <button
      type="button"
      class={() => `chip chip--${look()}${selected() ? " on" : ""}`}
      data-tone={stage}
      role="switch"
      aria-checked={() => (selected() ? "true" : "false")}
      aria-label={() => {
        const why = reason();
        return `${label} stage${why ? ` (${why})` : look() === "complete" ? ` (${STAGE_STATE.complete.label})` : ""}`;
      }}
      disabled={locked}
      ref={tip(tipText)}
      onclick={() => flip(() => run.toggleStage(stage))}
    >
      <span class="chip-bar" aria-hidden="true">
        {fill}
      </span>
      <span class="chip-row">
        <span class="chip-ico" aria-hidden="true">
          <Icon name={STAGE[stage].icon} />
        </span>
        <span class="chip-label" ref={clip()}>
          {label}
        </span>
        <Show
          when={() => end.shown().check}
          fallback={() => (
            <Show when={() => end.shown().text || end.shown().key.endsWith(":live")}>
              {() => (
                <span
                  class={() => `chip-tag handoff${end.shown().time ? " time" : ""}${end.out() ? " handoff-out" : ""}`}
                  data-tone={() => end.shown().tone ?? undefined}
                >
                  {() => (end.shown().key.endsWith(":live") ? [full.box, short.box] : end.shown().text)}
                </span>
              )}
            </Show>
          )}
        >
          {() => (
            <span class={() => (end.out() ? "chip-check handoff handoff-out" : "chip-check handoff")}>
              <Icon name="check" />
            </span>
          )}
        </Show>
      </span>
    </button>
  );
}

/** Start, Stop or Cancel. Never disabled: a refused start says why, in its tip and in the dial's note. */
export function RunButton(props: { app: App }): Node {
  const { app } = props;
  const { run, servers } = app;
  const busy = computed(() => run.running() || run.preparing());
  const idle = computed(() => !busy());
  const blocker = computed(() => (idle() && !servers.loading() ? app.blocker() : ""));
  // The running plan; otherwise the next one, its warmup adapted to the verified round trip.
  const eta = computed(() => {
    const config = app.preferences.config();
    const plan = run.running() ? run.plan() : null;
    const rtt = servers.views().get(servers.latencyServer() ?? "")?.latency.path?.rttMs ?? 0;
    const shown = plan ?? {
      ...config,
      duration: { ...config.duration, warmupMs: adaptWarmup(config.duration.warmupMs, rtt) },
    };
    return fmtDuration(timeline(shown).at(-1)?.end ?? 0, 0);
  });
  // The label, the skin and the stop mark change together, the moment the run's state does.
  return (
    <>
      <button
        type="button"
        class={() => `run-button${busy() ? " running" : ""}${run.preparing() ? " pending" : ""}`}
        aria-busy={() => (run.preparing() ? "true" : "false")}
        aria-disabled={() => (blocker() ? "true" : "false")}
        aria-describedby={() => (idle() ? "run-duration" : undefined)}
        ref={tip(blocker)}
        onclick={() => run.toggle()}
      >
        <span class="skin" aria-hidden="true" />
        <span class="skin stop" aria-hidden="true" />
        <span class="run-button-content">
          {() => busy() && <span class="stop-sq" aria-hidden="true" />}
          {() => runAction(run.preparing(), run.running(), run.phase())}
        </span>
        {() =>
          idle() && (
            <span class="duration" aria-hidden="true">
              ~{eta}
            </span>
          )
        }
      </button>
      {() =>
        idle() && (
          <span id="run-duration" class="sr-only">
            {() => (blocker() ? `${PHASE.blocked}: ${blocker()}` : `Estimated duration ${eta()}`)}
          </span>
        )
      }
    </>
  );
}
