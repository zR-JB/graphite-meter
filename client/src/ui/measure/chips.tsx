// The run key over the stage chips: a chip switches its stage for the next run and shows how it fares in the shown one.
import type { App } from "../../app/app";
import type { Shell } from "../shell/api";
import { planned, STAGES, type Stage } from "../../engine/run/schedule";
import { For, Show } from "../../lib/dom";
import type { Child } from "../../lib/jsx-runtime";
import { handoff } from "../../lib/motion";
import { computed, effect, untrack, type Read } from "../../lib/signal";
import { fmtDuration, fmtLatency, fmtProgress, fmtRate, fmtStageTime } from "../../view/format";
import {
  chipTag,
  PHASE,
  reasonLabel,
  STAGE,
  STAGE_STATE,
  TAGGED,
  type StageState,
  type Tone,
} from "../../view/words";
import { flip } from "../flip";
import { announceChanges } from "../kit/announce";
import { Icon } from "../kit/icon";
import { Roll } from "../kit/roll";
import { clip, tip } from "../kit/tip";
import { CHIPS } from "./copy";
import type { Live } from "./live";
import { Figure, painter } from "./paint";

/** How a chip looks: off, waiting, or the stage's state in the shown run. */
type Look =
  | "off"
  | "pending"
  | "warmup"
  | "active"
  | "recovering"
  | "complete"
  | "partial"
  | "failed";
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

export function StageTrack(props: {
  app: App;
  live: Live;
  away: Read<boolean>;
  children: Child;
}): Node {
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
          {(stage) => <Chip app={app} live={live} away={props.away} stage={stage()} />}
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

function Chip(props: { app: App; live: Live; away: Read<boolean>; stage: Stage }): Node {
  const { app, live, stage } = props;
  const { run } = app;
  const config = app.run.upcoming;
  const selected = computed(() => planned(config(), stage));
  const locked = computed(() => !run.canToggle(stage));
  const state = computed(() => live.states()[stage]);
  // A stage the next run includes but the shown run did not run waits for the next run.
  const nextRun = computed(() => selected() && state() === "skipped" && run.plan() !== null);
  const look = computed<Look>(() => (selected() ? LOOK[state()] : "off"));
  const tag = computed(() => chipTag(state(), selected(), nextRun()));
  // Why a locked chip cannot change; a settled one says nothing more than its check or tag.
  const reason = computed(() => {
    if (tag() !== null) return tag();
    if (!locked() || look() === "complete") return null;
    if (run.stage() === stage && RUNNING_LOOKS.has(look())) return STAGE_STATE[state()].label;
    return STAGE_STATE.upcoming.label;
  });
  const timed = computed(() => look() === "active" && live.budget() > 0);
  // A waiting chip shows its stage's length: the shown run's plan, or the next run's for a stage queued for it.
  const waiting = computed(() => selected() && look() === "pending");
  const lengthMs = computed(
    () => (nextRun() ? config() : (run.plan() ?? config())).duration[`${stage}Ms`],
  );
  const length = computed(() => fmtStageTime(lengthMs()));
  const end = handoff(
    () => {
      const now = look();
      if (now === "complete")
        return { key: `${now}:check`, check: true, text: "", time: false, tone: null };
      if (timed()) return { key: `${now}:live`, check: false, text: "", time: true, tone: null };
      // A new length rolls in place; only another kind of tag hands off.
      if (waiting() && !TAGGED.has(state()))
        return { key: `${now}:length`, check: false, text: length(), time: true, tone: null };
      const tone: Tone | null =
        now === "partial" || now === "recovering" ? "warn" : now === "failed" ? "err" : null;
      return {
        key: `${now}:${reason() ?? ""}`,
        check: false,
        text: reason() ?? "",
        time: false,
        tone,
      };
    },
    (view) => view.key,
  );
  const endKey = computed(() => end.shown().key);

  const value = computed(() => {
    if (stage === "latency") {
      const latency = run.results().latency;
      return latency ? fmtLatency(latency.reportedMs) : "";
    }
    const results = run.results();
    const down = results.bidirectional?.down?.reportedBytesPerSec;
    const up = results.bidirectional?.up?.reportedBytesPerSec;
    const bytes =
      stage === "bidirectional"
        ? down != null && up != null
          ? down + up
          : null
        : results[stage]?.reportedBytesPerSec;
    return bytes == null ? "" : fmtRate(bytes, live.rateUnits());
  });
  const failure = computed(() => {
    const found = run
      .details()
      ?.failures.find(
        (each) => each.stage === stage && (each.scope === "latency") === (stage === "latency"),
      );
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
      now === "warmup" && "is-warmup",
      now === "failed" && "is-failed",
      now === "partial" && "is-partial",
      now === "recovering" && "is-stalled",
      timed() && "is-live",
    ]
      .filter(Boolean)
      .join(" ");
  });
  // The bar: progress while its stage runs, the share a stopped one reached, full once settled; only a clocked bar draws every frame.
  const clocked = computed(() => {
    const now = look();
    return (
      now === "active" ||
      now === "recovering" ||
      (state() === "stopped" && live.endedIn() === stage)
    );
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
    // A plain transform, not a custom property: Chrome then restyles the fill from its previous style.
    if (!(Math.abs(next - progress) < 5e-4)) fill.style.transform = `scaleX(${(progress = next)})`;
    if (timed.peek() && Math.round(clock / 100) !== tenths) {
      tenths = Math.round(clock / 100);
      full.set(fmtProgress(clock, length));
      short.set(fmtDuration(clock, 1));
    }
    return clocked.peek() && live.active(now);
  };
  const wake = painter(draw, props.away, live.beat);
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

  return (
    <button
      type="button"
      class={() => `chip chip--${look()}`}
      data-tone={stage}
      role="switch"
      aria-checked={() => (selected() ? "true" : "false")}
      aria-label={() => {
        const why = reason() ?? (look() === "complete" ? STAGE_STATE.complete.label : null);
        return `${STAGE[stage].label} stage${why ? ` (${why})` : ""}`;
      }}
      aria-disabled={() => (locked() ? "true" : "false")}
      ref={tip(tipText)}
      onclick={() => locked() || flip(() => run.toggleStage(stage))}
    >
      <span class="chip-bar" aria-hidden="true">
        {fill}
      </span>
      <span class="chip-row">
        <span class="chip-ico" aria-hidden="true">
          <Icon name={STAGE[stage].icon} />
        </span>
        <span class="chip-label" ref={clip()}>
          {STAGE[stage].short}
        </span>
        <Show
          when={() => end.shown().check}
          fallback={() => (
            <Show when={() => end.shown().text || end.shown().key.endsWith(":live")}>
              {() => (
                <span
                  class={() =>
                    `chip-tag handoff${end.shown().time ? " time" : " caps"}${end.out() ? " handoff-out" : ""}`
                  }
                  data-tone={() => end.shown().tone ?? undefined}
                >
                  {() => {
                    const key = endKey();
                    if (key.endsWith(":live")) return [full.box, short.box];
                    if (key.endsWith(":length")) return <Roll text={length} rank={lengthMs} />;
                    return untrack(() => end.shown().text);
                  }}
                </span>
              )}
            </Show>
          )}
        >
          {() => (
            <span
              class={() => (end.out() ? "chip-check handoff handoff-out" : "chip-check handoff")}
            >
              <Icon name="check" />
            </span>
          )}
        </Show>
      </span>
    </button>
  );
}

/** Start, Stop or Cancel; a blocked start stays focusable and says why in its description, its tip and the dial's note. */
export function RunButton(props: { app: App; shell: Shell }): Node {
  const { app } = props;
  const { run, servers } = app;
  const busy = computed(() => run.running() || run.preparing());
  const idle = computed(() => !busy());
  const blocker = computed(() => (idle() && !servers.loading() ? app.blocker() : ""));
  const eta = computed(() => fmtDuration(app.plannedMs(), 0));
  // The label, the skin and the stop mark change together, the moment the run's state does.
  return (
    <>
      <button
        type="button"
        class={() => `run-button${busy() ? " running" : ""}${run.preparing() ? " pending" : ""}`}
        aria-disabled={() => (blocker() ? "true" : "false")}
        aria-describedby={() => (idle() ? "run-duration" : undefined)}
        ref={tip(blocker)}
        onclick={() => props.shell.toggleRun()}
      >
        <span class="skin" aria-hidden="true" />
        <span class="skin stop" aria-hidden="true" />
        <span class="run-button-content">
          {() => busy() && <span class="stop-sq" aria-hidden="true" />}
          {props.shell.runWord}
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
