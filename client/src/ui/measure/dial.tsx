// The dial; per frame its painter draws the needle on a canvas and writes a transform, a text node and a class.
import type { App } from "../../app/app";
import { CHECK_FAILED } from "../../engine/net/checks";
import { STAGES, type Stage } from "../../engine/run/schedule";
import { For, Show } from "../../lib/dom";
import { pageMs } from "../../lib/frame";
import { HANDOFF_OUT_MS, handoff, reducedMotion } from "../../lib/motion";
import { computed, effect, type Read } from "../../lib/signal";
import {
  fmtDuration,
  fmtMs,
  fmtSpeed,
  fmtTick,
  MISSING,
  rateUnit,
  toRateUnit,
  withUnit,
} from "../../view/format";
import { latencyCeilingMs } from "../../view/scales";
import { OUTCOME, PHASE, POPULATION, reasonLabel, STAGE, statusLabel, TIP } from "../../view/words";
import { announceChanges } from "../kit/announce";
import { Icon } from "../kit/icon";
import { tip } from "../kit/tip";
import { serverEvidence } from "./cards-model";
import { DIAL, PHASE_HINT } from "./copy";
import {
  ARC_START,
  ARC_SWEEP,
  arcLabel,
  beadKinds,
  DIAL_LABELS,
  dialLayout,
  gaugeFraction,
  gaugeValue,
  headlineArc,
  Needle,
  reachMs,
  RESCALE_MS,
  resultArcs,
  resultSweepMs,
  SWEEP_DEG,
  type DialLayout,
  type ResultArc,
} from "./dial-model";
import type { Live } from "./live";
import { Figure, inView, measure, painter } from "./paint";

/** A result leaves as its arcs drain back to zero, so the next run starts empty. */
const RESULT_DRAIN_MS = 160;
const TRANSFER: readonly string[] = ["download", "upload", "bidirectional"];
const LABELED: readonly string[] = ["latency", "warmup", ...TRANSFER, "complete"];

let dials = 0;

export function Dial(props: {
  app: App;
  live: Live;
  away: Read<boolean>;
  hung: Read<boolean>;
  lens: Node;
}): HTMLElement {
  const { app, live } = props;
  const { run } = app;
  const phase = run.phase;
  const units = live.rateUnits;
  const unit = computed(() => rateUnit(units(), units().tier ?? 0));
  const rate = (bytesPerSec: number) =>
    fmtSpeed(toRateUnit(bytesPerSec, units(), units().tier ?? 0));
  const gaugeScale = computed(() => live.scales().gaugeBytesPerSec);

  // A finished run's arcs read what the lens shows: every server together, or the one chosen under the dial.
  const shown = computed(() => {
    const id = live.lens();
    const details = run.details();
    return id && details ? serverEvidence(details, id) : run.result();
  });
  const arcs = computed(() => (phase() === "complete" ? resultArcs(shown()) : []));
  const headline = computed(() => headlineArc(arcs()));
  // A failed stage, or a finished run that measured nothing, has no value to show.
  const unusable = computed(() => {
    const stage = run.stage();
    return (
      (stage !== null && live.states()[stage] === "failed") ||
      (phase() === "complete" && !arcs().length && !run.result()?.latency)
    );
  });
  // A stage's warmup already reads its stage, muted: the checked round trip, or the rate nothing counts yet.
  const warming = computed(() => phase() === "warmup" && run.running());
  const reads = computed<string>(() => (warming() ? (run.stage() ?? phase()) : phase()));
  // What the needle points at: the round trip on the millisecond scale, the stage's own rate, or nothing.
  const pointing = computed<"rate" | "ms" | null>(() => {
    const now = reads();
    if (now === "latency") return "ms";
    if (TRANSFER.includes(now)) return live.rateStage() !== null ? "rate" : null;
    return now === "complete" && live.completedRtt() !== null ? "ms" : null;
  });
  const visible = computed(() => !unusable() && pointing() !== null);
  // The millisecond scale: the shared axis once a bucket measured it, else a ladder step over the shown round trip.
  const scaleMs = computed(() => {
    const completed = live.completedRtt();
    const buckets = live.buckets();
    const measured =
      completed !== null
        ? buckets.some((bucket) => bucket.medianRttMs !== null)
        : phase() === "latency" && buckets.at(-1)?.phase === "latency";
    return measured ? live.latencyAxis() : latencyCeilingMs([completed ?? live.replies().rttMs]);
  });

  // Labels change in place with the scale or the unit; only a switch between rate and time ticks hands off.
  const ticks = handoff(
    () => {
      if (unusable() || !LABELED.includes(phase())) return [] as string[];
      if (live.msScale()) return DIAL_LABELS.map((f) => fmtTick(scaleMs() * f));
      return DIAL_LABELS.map((f) =>
        fmtTick(toRateUnit(gaugeValue(f, gaugeScale()), units(), units().tier ?? 0)),
      );
    },
    (labels) => (labels.length ? live.msScale.peek() : null),
    (leaving) => (leaving.length ? HANDOFF_OUT_MS : 0),
  );

  // A result hands off to the live readout and back; the unit is not part of either, so a switch relabels in place.
  const hero = handoff(
    () => {
      const arc = headline();
      return {
        terminal: arc && { arc, value: rate(arc.bytesPerSec), unit: unit() },
        arcs: arcs(),
        key: arc ? `${arc.stage}:${run.result()?.startedAt ?? ""}:${live.lens()}` : "live",
      };
    },
    (view) => view.key,
    (leaving) => (leaving.arcs.length ? RESULT_DRAIN_MS : HANDOFF_OUT_MS),
  );
  // The stage's name and figure hand off apart: the name once per stage, the figure on its first value, so a lasting dash never blinks.
  const stageName = handoff<Stage | null>(
    () => (run.running() ? run.stage() : live.completedRtt() !== null ? "latency" : null),
    (stage) => stage,
    (leaving) => (leaving === null ? 0 : HANDOFF_OUT_MS),
  );
  const reading = computed<"rate" | "ms" | null>(() => {
    if (unusable()) return null;
    const now = reads();
    if (now === "latency") return live.replies().lost ? null : "ms";
    if (TRANSFER.includes(now)) return live.rateStage() !== null ? "rate" : null;
    return now === "complete" && !headline() && live.completedRtt() !== null ? "ms" : null;
  });
  const figure = handoff(
    () => {
      const shown = reading();
      return {
        reading: shown,
        unit: shown === "ms" ? "ms" : shown === "rate" ? unit() : "",
        stage: run.running() ? run.stage() : null,
      };
    },
    (view) => (view.reading === null ? MISSING : view.stage),
    // A first value takes the dash's place at once; only a value hands off to the next.
    (leaving) => (leaving.reading === null ? 0 : HANDOFF_OUT_MS),
  );
  // A screen reader hears the unanimated value: the receiver's accounting, or the round trip shown.
  const spoken = computed(() => {
    const arc = headline();
    if (arc) return `${rate(arc.bytesPerSec)} ${unit()}, ${arcLabel(arc)}`;
    const shown = reading();
    if (shown === "ms") return withUnit(fmtMs(live.completedRtt() ?? live.replies().rttMs), "ms");
    const sample = run.live();
    if (shown === "rate" && sample)
      return withUnit(rate((sample.down ?? 0) + (sample.bridgedUp ?? sample.up ?? 0)), unit());
    return MISSING;
  });

  // The note under the ring: a check, a refused start, how the run ended, a stall or what comes next; only a new kind hands off.
  const unanswered = computed(() => {
    if (phase() !== "latency" || !live.replies().lost) return null;
    const buckets = live.buckets();
    const newest = buckets.at(-1)?.endT ?? 0;
    const answered =
      buckets.findLast((bucket) => bucket.phase === "latency" && bucket.medianRttMs !== null)
        ?.endT ?? run.shownAt();
    return newest - answered >= 1_000 ? newest - answered : null;
  });
  // A failed check names the path the first selected server's check failed on; else why the start was refused.
  const refusal = computed(() => {
    const preparation = live.preparation();
    const detail = run.startError() || app.blocker();
    if (preparation === "blocked") return { status: PHASE.blocked, detail };
    if (preparation !== "failed") return null;
    const view = app.servers.views().get(app.servers.selected()[0] ?? "");
    const throughput = view?.throughput.state === "failed";
    const latency = live.latencyEnabled() && view?.latency.state === "failed";
    if (!throughput && !latency) return { status: PHASE["start-failed"], detail };
    return { status: CHECK_FAILED, detail: DIAL.unavailable(throughput, latency) };
  });
  const ending = computed(() => {
    if (phase() === "aborted") return { status: PHASE.stopped, tone: "", hint: DIAL.restart };
    const result = run.result();
    if (phase() === "complete" && result && result.outcome !== "complete") {
      const named = (status: string) =>
        STAGES.filter((stage) => result.stages[stage] === status).map(
          (stage) => STAGE[stage].label,
        );
      // Loaded latency that failed leaves its transfer stage complete but the run partial.
      const loaded = STAGES.filter(
        (stage) =>
          stage !== "latency" &&
          result.multiServer.failures.some(
            (each) => each.scope === "latency" && each.stage === stage,
          ),
      ).map((stage) => POPULATION[stage]);
      return {
        status: OUTCOME[result.outcome].label,
        tone: "warn",
        hint: DIAL.unfinished(named("failed"), [...named("partial"), ...loaded]),
      };
    }
    if (phase() !== "error") return null;
    const error = run.error();
    return {
      status: error ? reasonLabel(error.reason) : DIAL.unknownError,
      tone: "error",
      hint: DIAL.retry,
    };
  });
  const note = handoff(
    (): { status?: string; tone?: string; hint?: string; tip?: string } => {
      const refused = refusal();
      const ended = ending();
      if (run.preparing())
        return { status: statusLabel(live.preparation(), phase(), undefined), tone: "preparation" };
      if (refused) return { status: refused.status, tone: "error", hint: refused.detail };
      if (ended) return { status: ended.status, tone: ended.tone, hint: ended.hint };
      const quiet = run.live()?.quietMs;
      if (quiet != null) return { hint: DIAL.noData(fmtDuration(quiet, 0)), tip: TIP.noData };
      const silent = unanswered();
      if (silent !== null)
        return { hint: DIAL.noReplies(fmtDuration(silent, 0)), tip: TIP.noReplies };
      const known = PHASE_HINT[phase()];
      return known ? { hint: known.text, tip: known.tip } : {};
    },
    (view) => `${view.status}|${view.tip ?? view.hint}`,
    (leaving) => (leaving.status || leaving.hint ? HANDOFF_OUT_MS : 0),
  );
  // Preparation speaks only through its outcome; the cards announce a completed run.
  announceChanges(() => {
    const refused = refusal();
    const ended = ending();
    if (refused) return [refused.status, refused.detail].filter(Boolean).join(": ");
    if (ended) return [ended.status, ended.hint].filter(Boolean).join(": ");
    const now = phase();
    if (run.preparing() || now === "idle" || now === "connecting" || now === "complete") return "";
    return now === "warmup" ? PHASE.warmup : STAGE[now as Stage].label;
  });

  const face = (<div class="gauge-face" />) as HTMLElement;
  const footer = (<div class="gauge-footer handoff" />) as HTMLElement;
  const sizes = measure({ face, footer });
  const layout = computed(() => {
    const { width, height } = sizes.face();
    return dialLayout(width, height, props.hung() ? sizes.footer().height : 0);
  });
  effect(() => {
    const { height, noteTop, noteWidth } = layout();
    const hung = props.hung();
    footer.style.top = hung ? `calc(100% - ${height - noteTop}px)` : "";
    footer.style.setProperty(
      "--note-width",
      hung && ticks.shown().length > 1 ? `${noteWidth}px` : "",
    );
  });
  effect(() => footer.classList.toggle("handoff-out", note.out()));

  const dial = (<div class="gauge-dial" />) as HTMLElement;
  dial.style.setProperty("--drain", `${RESULT_DRAIN_MS}ms`);
  dial.style.setProperty("--rescale", `${RESCALE_MS}ms`);
  const seen = inView(dial, true);
  // An unseen dial snaps, and runs no transitions.
  const motion = computed(() => seen() && !reducedMotion());
  effect(() => dial.classList.toggle("motion", motion()));
  // From its warmup on, the track takes a faint tint of the running stage's hue.
  effect(() => {
    const stage = run.running() ? run.stage() : null;
    dial.style.setProperty("--ring", stage ? `var(--phase-${stage})` : "");
  });

  const needle = new Needle();
  // One bitmap for the arc, its start cap and its head: cheaper per frame than moving vector art in every engine.
  const canvas = (<canvas />) as HTMLCanvasElement;
  const pen = canvas.getContext("2d")!;
  // The head's place, where a reply's ring widens.
  const head = (<div class="live-head" />) as HTMLElement;
  const needleLayer = (
    <div class="live" aria-hidden="true">
      {canvas}
      {head}
    </div>
  ) as HTMLElement;
  effect(() => needleLayer.classList.toggle("warming", warming()));
  // Between runs the needle keeps the hue it last showed; a finished run's latency takes its own.
  effect(() => {
    const stage = run.running() ? run.stage() : live.completedRtt() !== null ? "latency" : null;
    if (stage) needleLayer.style.setProperty("--accent", `var(--phase-${stage})`);
  });

  const number = new Figure("dial-figure", 5);
  number.set(MISSING);
  const value = (
    <span
      class={() =>
        `gauge-value handoff${figure.out() ? " handoff-out" : ""}${figure.shown().reading && !warming() ? "" : " quiet"}`
      }
    >
      {number.box}
    </span>
  ) as HTMLElement;

  // What the painter last wrote, so it writes only changes.
  let drawn = Number.NaN;
  let inked = "";
  let lit = false;
  let written = Number.NaN;
  let scale = 0;
  let box = [0, 0, 0, 0];
  function dot(x: number, y: number, r: number): void {
    pen.beginPath();
    pen.arc(x, y, r, 0, 2 * Math.PI);
    pen.fill();
  }
  function draw(now: number): boolean {
    const kind = pointing.peek();
    const target =
      kind === "rate"
        ? gaugeFraction(live.down.at(now) + live.up.at(now), gaugeScale.peek()) * SWEEP_DEG
        : kind === "ms"
          ? Math.min(1, Math.max(0, live.rtt.at(now) / scaleMs.peek())) * SWEEP_DEG
          : null;
    const degrees = needle.angle(now, visible.peek() ? target : null);
    // The hue glides on a stage change, so the needle reads it from its layer's style.
    const ink = getComputedStyle(needleLayer).color;
    if (devicePixelRatio !== scale) {
      scale = devicePixelRatio;
      canvas.width = Math.round(box[2]! * scale);
      canvas.height = Math.round(box[3]! * scale);
      drawn = Number.NaN;
    }
    if (!(Math.abs(degrees - drawn) < 0.01) || ink !== inked) {
      drawn = degrees;
      inked = ink;
      const { center, radius, arcWidth, headRadius } = layout.peek();
      const end = ARC_START + (degrees * Math.PI) / 180;
      pen.setTransform(scale, 0, 0, scale, -box[0]! * scale, -box[1]! * scale);
      pen.clearRect(box[0]!, box[1]!, box[2]!, box[3]!);
      pen.fillStyle = pen.strokeStyle = ink;
      pen.lineWidth = arcWidth;
      // At rest the head covers the first degree, and the start while the arc is shorter than the head.
      if (degrees > 1) {
        pen.beginPath();
        pen.arc(center.x, center.y, radius, ARC_START, end);
        pen.stroke();
      }
      if ((degrees * Math.PI * radius) / 180 >= headRadius + arcWidth / 2)
        dot(
          center.x + Math.cos(ARC_START) * radius,
          center.y + Math.sin(ARC_START) * radius,
          arcWidth / 2,
        );
      dot(center.x + Math.cos(end) * radius, center.y + Math.sin(end) * radius, headRadius);
      head.style.transform = `translate(${Math.cos(end) * radius}px, ${Math.sin(end) * radius}px)`;
    }
    const on = needle.lit(visible.peek(), motion.peek());
    if (on !== lit) needleLayer.classList.toggle("visible", (lit = on));
    // The figure reads the shown view, glided; its text changes only when what it reads does.
    const shown = figure.shown.peek().reading;
    const page = units.peek();
    const amount =
      shown === "rate"
        ? toRateUnit(live.down.at(now) + live.up.at(now), page, page.tier ?? 0)
        : (live.completedRtt.peek() ?? live.rtt.at(now));
    const key = shown === null ? -1 : Math.round(amount * (shown === "ms" ? 10 : 100));
    if (key !== written) {
      written = key;
      number.set(shown === null ? MISSING : shown === "ms" ? fmtMs(amount) : fmtSpeed(amount));
    }
    return needle.moving(now) || live.active(now) || fading;
  }
  const wake = painter(draw, props.away);
  // A hue that glides, on a stage or theme change, is drawn until it lands.
  let fading = false;
  const fade = (event: TransitionEvent, now: boolean) => {
    if (event.target !== needleLayer || event.propertyName !== "color") return;
    fading = now;
    wake();
  };
  needleLayer.addEventListener("transitionrun", (event) => fade(event, true));
  needleLayer.addEventListener("transitionend", (event) => fade(event, false));
  needleLayer.addEventListener("transitioncancel", (event) => fade(event, false));
  effect(() => {
    needle.update(
      {
        visible: visible(),
        hold: phase() === "complete" && arcs().length > 0,
        motion: motion(),
        course: `${gaugeScale()}:${scaleMs()}`,
      },
      pageMs(),
    );
    wake();
  });
  effect(() => {
    live.moved();
    run.running();
    figure.shown();
    units();
    wake();
  });
  effect(() => {
    const { center, radius, headRadius } = layout();
    // The whole pixels around the ring and the head's widest reach.
    const reach = radius + headRadius + 2;
    const [x, y] = [Math.floor(center.x - reach), Math.floor(center.y - reach)];
    box = [x, y, Math.ceil(center.x + reach) - x, Math.ceil(center.y + reach) - y];
    Object.assign(canvas.style, {
      left: `${box[0]}px`,
      top: `${box[1]}px`,
      width: `${box[2]}px`,
      height: `${box[3]}px`,
    });
    scale = 0;
    Object.assign(head.style, { left: `${center.x}px`, top: `${center.y}px` });
    head.style.setProperty("--head-r", `${headRadius}px`);
    drawn = Number.NaN;
    wake(true);
  });

  // One ring per idle reply, never two at once: a steady link is seen to answer, a stalled one holds still.
  let beating = false;
  const reply = computed(() =>
    phase() === "latency"
      ? (live.buckets().findLast((bucket) => bucket.medianRttMs !== null)?.startT ?? null)
      : null,
  );
  effect(() => {
    if (reply() === null || beating || !motion.peek()) return;
    beating = true;
    // A box, not a circle in the head's SVG, so its ring widens and fades on the compositor.
    const ripple = document.createElement("span");
    ripple.className = "ripple";
    const done = () => {
      ripple.remove();
      beating = false;
    };
    ripple.addEventListener("animationend", done);
    ripple.addEventListener("animationcancel", done);
    head.prepend(ripple);
  });

  const art = (
    <svg class="dial-art" aria-hidden="true">
      <g fill="none" stroke-linecap="round">
        <path class="track" />
        <path class="ticks" />
      </g>
    </svg>
  ) as SVGSVGElement;
  effect(() => {
    const { width, height, track, ticks, arcWidth } = layout();
    sizeArt(art, width, height);
    const [trackPath, tickPath] = art.querySelectorAll("path");
    trackPath!.setAttribute("d", track);
    trackPath!.setAttribute("stroke-width", String(arcWidth));
    tickPath!.setAttribute("d", ticks);
  });

  const ids = `dial-${++dials}`;
  let from = 0;
  const shownArcs = computed(() => hero.shown().arcs);
  const result = (
    <Show when={() => shownArcs().length > 0}>
      {() => {
        // The result takes the ring over where the needle stands, and its sweep runs on from there.
        from = needle.takeover(motion.peek(), pageMs());
        dial.style.setProperty("--from", String(from));
        dial.style.setProperty("--sweep-ms", `${resultSweepMs(from)}ms`);
        wake();
        return (
          <ResultLayer
            arcs={shownArcs}
            out={hero.out}
            layout={layout}
            gaugeScale={gaugeScale}
            label={(arc) => `${arcLabel(arc)}\n${rate(arc.bytesPerSec)} ${unit()}`}
            masks={ids}
            from={from}
          />
        );
      }}
    </Show>
  );
  dial.append(art, result, needleLayer);

  const labels = (
    <Show when={() => ticks.shown().length > 1}>
      {() => (
        <div
          class={() => (ticks.out() ? "gauge-ticks handoff handoff-out" : "gauge-ticks handoff")}
          aria-hidden="true"
        >
          {DIAL_LABELS.map((_, index) => {
            const label = (
              <span class="gauge-tick">{() => ticks.shown()[index] ?? ""}</span>
            ) as HTMLElement;
            effect(() => {
              const point = layout().labels[index]!;
              label.dataset.anchorX = point.anchorX;
              label.dataset.anchorY = point.anchorY;
              label.style.left = `${point.x}px`;
              label.style.top = `${point.y}px`;
            });
            return label;
          })}
        </div>
      )}
    </Show>
  );

  const terminal = computed(() => hero.shown().terminal);
  const readout = (
    <div class={() => (hero.out() ? "metric-wrap handoff handoff-out" : "metric-wrap handoff")}>
      <div
        class={() =>
          `hero${terminal() ? " terminal" : ""}${sizes.face().height < 180 ? " short" : ""}`
        }
      >
        <Show
          when={terminal}
          fallback={() => (
            <div class="terminal-readout" aria-hidden="true">
              <Show when={stageName.shown}>
                {(stage) => (
                  <span
                    class={() =>
                      stageName.out()
                        ? "terminal-direction handoff handoff-out"
                        : "terminal-direction handoff"
                    }
                    data-tone={stage}
                  >
                    <span class="tone-icon" aria-hidden="true">
                      {() => <Icon name={STAGE[stage()].icon} />}
                    </span>
                    {() => STAGE[stage()].label}
                  </span>
                )}
              </Show>
              {value}
              <span
                class={() =>
                  figure.out() ? "gauge-unit handoff handoff-out" : "gauge-unit handoff"
                }
              >
                {() => figure.shown().unit}
              </span>
            </div>
          )}
        >
          {(shown) => (
            <div
              class={() => (shown().arc.dashed ? "terminal-readout partial" : "terminal-readout")}
              aria-hidden="true"
            >
              <span class="terminal-direction" data-tone={() => shown().arc.direction}>
                <span class="tone-icon" aria-hidden="true">
                  {() => <Icon name={STAGE[shown().arc.direction].icon} />}
                </span>
                {() => STAGE[shown().arc.direction].label}
              </span>
              <span class="terminal-number">{() => shown().value}</span>
              <span class="terminal-unit">{() => shown().unit}</span>
              <Show when={() => shown().arc.dashed}>
                {() => (
                  <span class="terminal-partial" data-tone="warn">
                    {() => arcLabel(shown().arc)}
                  </span>
                )}
              </Show>
            </div>
          )}
        </Show>
        <span class="sr-only">
          <span class="spoken">{spoken}</span>
        </span>
      </div>
    </div>
  ) as HTMLElement;
  // Only the readout reads the face's size: set on it, a resize restyles its few elements, not the whole dial.
  effect(() => {
    const { center, height, faceMin } = layout();
    readout.style.setProperty("--gauge-center-offset", `${center.y - height / 2}px`);
    readout.style.setProperty("--face-min", `${faceMin}px`);
  });
  face.append(dial, labels, readout);

  footer.append(
    <Show when={() => !!(note.shown().status || note.shown().hint)}>
      {() => (
        <div class="gauge-notes">
          <Show when={() => note.shown().status}>
            {(status) => (
              <span class={() => `gauge-status ${note.shown().tone ?? ""}`.trimEnd()}>
                {status}
              </span>
            )}
          </Show>
          <Show when={() => note.shown().hint}>
            {(hint) => (
              <span class="gauge-hint" ref={tip(() => note.shown().tip ?? "")}>
                {hint}
              </span>
            )}
          </Show>
        </div>
      )}
    </Show>,
  );

  return (
    <div class={() => (props.hung() ? "dial hung" : "dial")} data-flip="dial">
      {props.lens}
      {face}
      {footer}
    </div>
  ) as HTMLElement;
}

function sizeArt(art: SVGSVGElement, width: number, height: number): void {
  art.setAttribute("width", String(width));
  art.setAttribute("height", String(height));
  art.setAttribute("viewBox", `0 0 ${width} ${height}`);
}

const angle = (fraction: number) => ARC_START + ARC_SWEEP * fraction;

/** Every result's arc, longest underneath, kept by stage so a scale change glides them without replaying the sweep. */
function ResultLayer(props: {
  arcs: Read<ResultArc[]>;
  out: Read<boolean>;
  layout: Read<DialLayout>;
  gaugeScale: Read<number>;
  label: (arc: ResultArc) => string;
  masks: string;
  from: number;
}): Node {
  const { arcs, layout } = props;
  const fractions = computed(() =>
    arcs().map((arc) =>
      Math.min(1, Math.max(0, gaugeFraction(arc.bytesPerSec, props.gaugeScale()))),
    ),
  );
  const kinds = computed(() => beadKinds(fractions(), layout().radius, layout().headRadius));
  const fractionOf = (arc: ResultArc) => fractions()[arcs().indexOf(arc)] ?? 0;
  const sweepMs = resultSweepMs(props.from);
  const art = (
    <svg class="dial-art" aria-hidden="true">
      <g fill="none" stroke-linecap="round">
        <For each={arcs} by={(arc) => arc.stage}>
          {(arc) => {
            const mask = `${props.masks}-${arc().stage}`;
            const path = (
              <path
                class="result-arc"
                data-tone={arc().stage}
                d={() => layout().track}
                stroke-width={() => layout().arcWidth}
                mask={() => (arc().dashed ? `url(#${mask})` : undefined)}
              />
            ) as SVGPathElement;
            effect(() => {
              path.style.setProperty("--arc", `${layout().radius * ARC_SWEEP}px`);
              path.style.setProperty("--fraction", String(fractionOf(arc())));
            });
            // A one-direction arc shows through a still dash mask, as a mask's paint may lag its style; round caps widen each dash.
            return [
              <Show when={() => arc().dashed}>
                {() => (
                  <mask
                    id={mask}
                    maskUnits="userSpaceOnUse"
                    x="0"
                    y="0"
                    width={() => layout().width}
                    height={() => layout().height}
                  >
                    <path
                      d={() => layout().track}
                      stroke="white"
                      stroke-width={() => layout().arcWidth}
                      stroke-dasharray={() => `${layout().arcWidth * 0.5} ${layout().arcWidth * 2}`}
                    />
                  </mask>
                )}
              </Show>,
              path,
            ];
          }}
        </For>
      </g>
      <For each={arcs} by={(arc) => arc.stage}>
        {(arc) => {
          const group = (<g class="head result" />) as SVGGElement;
          effect(() => {
            group.style.transform = `rotate(${angle(fractionOf(arc()))}rad)`;
          });
          const bead = (
            <circle
              class={() => {
                const kind = kinds()[arcs().indexOf(arc())];
                const ring = arc().dashed ? " hollow" : kind === "stacked" ? " stacked" : "";
                return `bead${ring}${kind === "split" ? " split" : ""}`;
              }}
              data-tone={arc().stage}
              cx={() => layout().radius}
              r={() => layout().headRadius - (arc().dashed ? 1 : 0)}
            />
          ) as SVGCircleElement;
          // Each bead pops on the spring as the sweep's front reaches it.
          bead.style.setProperty("--at", `${reachMs(fractionOf(arc()), props.from, sweepMs)}ms`);
          group.append(bead);
          return (
            <g transform={() => `translate(${layout().center.x} ${layout().center.y})`}>{group}</g>
          );
        }}
      </For>
    </svg>
  ) as SVGSVGElement;
  effect(() => sizeArt(art, layout().width, layout().height));
  return (
    <div class={() => (props.out() ? "result-layer handoff handoff-out" : "result-layer handoff")}>
      {art}
      <For each={arcs} by={(arc) => arc.stage}>
        {(arc) => {
          // Pointer-only: the cards and the announcement carry these values.
          const target = (
            <span
              class="head-target"
              aria-hidden="true"
              tabindex="-1"
              ref={tip(() => props.label(arc()))}
            />
          ) as HTMLElement;
          effect(() => {
            const { center, radius } = layout();
            const at = angle(fractionOf(arc()));
            target.style.left = `${center.x + Math.cos(at) * radius}px`;
            target.style.top = `${center.y + Math.sin(at) * radius}px`;
          });
          return target;
        }}
      </For>
    </div>
  );
}
