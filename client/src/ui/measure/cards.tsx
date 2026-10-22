// The result cards: the live console's carry strips and follow the frame clock; History's show a saved result.
import type { App } from "../../app/app";
import type { RunResult } from "../../engine/run/result";
import { STAGES, type Stage, type TransferStage } from "../../engine/run/schedule";
import { For, Show } from "../../lib/dom";
import { computed, effect, untrack, type Read } from "../../lib/signal";
import {
  fmtMs,
  fmtRate,
  fmtSpeed,
  MISSING,
  rateUnit,
  toRateUnit,
  type RateUnits,
} from "../../view/format";
import { replyTrackMs } from "../../view/scales";
import { STAGE, TIP, type StageState } from "../../view/words";
import { announce } from "../kit/announce";
import { Icon } from "../kit/icon";
import { tipGroup } from "../kit/rove";
import { clip, term, tip } from "../kit/tip";
import {
  cardReason,
  liveCard,
  resultSentence,
  serverEvidence,
  serverIssues,
  settled,
  settledCard,
  shapeSoFar,
  shownEvidence,
  type CardModel,
  type Evidence,
  type Issue,
  type LiveEvidence,
} from "./cards-model";
import { CARDS } from "./copy";
import type { Live } from "./live";
import { Figure, painter, setText } from "./paint";
import { StageGraph, type Head } from "./strip";

interface Figures {
  num: Figure;
  down: Figure;
  up: Figure;
  combined: Figure | null;
}

/** One stage's card. The headline is re-made, rising in, when the lens shows another server. */
function Card(props: {
  card: Read<CardModel>;
  reason: Read<string>;
  lens: Read<string>;
  strip?: () => Node | null;
}): {
  element: HTMLElement;
  figures: Figures;
} {
  const { card } = props;
  const stage = card.peek().stage;
  const figures: Figures = {
    num: new Figure("headline-figure"),
    down: new Figure("headline-figure"),
    up: new Figure("headline-figure"),
    combined: null,
  };
  const texts = {
    num: document.createTextNode(""),
    down: document.createTextNode(""),
    up: document.createTextNode(""),
  };
  const status = computed(() => card().status);
  const quiet = computed(() => card().quiet);
  const live = computed(() => card().live);
  const figure = computed(() => card().figure);
  const down = computed(() => card().pair?.down ?? MISSING);
  const up = computed(() => card().pair?.up ?? MISSING);
  const pair = computed(() => !!card().pair && !quiet());
  const unit = computed(() => (quiet() ? "" : card().unit));
  const spoken = computed(() => card().spoken ?? "");
  // A figure waiting for its value is a soft dash, like every dash that waits.
  const waiting = computed(() => figure() === MISSING && !live() && !pair());
  effect(() => setText(texts.num, figure()));
  effect(() => setText(texts.down, down()));
  effect(() => setText(texts.up, up()));
  // While its stage runs the frame clock writes the figures.
  const painted = computed(() => live() !== null);
  const lane = (dir: "download" | "upload", text: Text, box: Figure) => (
    <span class="pair" data-tone={dir}>
      <span class="arrow" aria-hidden="true">
        {dir === "download" ? "↓" : "↑"}
      </span>
      <span class="sr-only">{STAGE[dir].short}</span>
      <span class="num">{() => (painted() ? box.box : text)}</span>
    </span>
  );
  const headline = () => (
    <span
      class={() =>
        `headline enter${quiet() ? " quiet" : ""}${waiting() ? " waiting" : ""}${card().warming ? " warming" : ""}`
      }
      aria-hidden={() => (spoken() ? "true" : undefined)}
    >
      {() =>
        pair() ? (
          [lane("download", texts.down, figures.down), lane("upload", texts.up, figures.up)]
        ) : (
          <span class="num">{() => (painted() ? figures.num.box : texts.num)}</span>
        )
      }
      {() => unit() && <span class="unit">{unit}</span>}
    </span>
  );
  const word = computed(() => card().word);
  // A running bidirectional stage's combined rate is painted on the frame clock.
  const facts = card.peek().facts.map((fact, index) => {
    const value = computed(() => card().facts[index]?.value ?? MISSING);
    const combined = computed(() => fact.key === "combined" && live() === "pair");
    const shown = new Figure("fact-figure");
    if (fact.key === "combined") figures.combined = shown;
    effect(() => {
      if (!combined()) shown.set(value());
    });
    return (
      <div>
        <dt ref={tip(computed(() => card().facts[index]?.tip ?? ""))}>{fact.label}</dt>
        <dd class={() => (value() === MISSING && !combined() ? "quiet" : undefined)}>
          {shown.box}
        </dd>
      </div>
    );
  });
  const unknown = computed(() => card().facts.every((fact) => fact.value === MISSING));
  // The quiet line: a failure's reason, else the wire rate, else latency's jitter; then time without data.
  const reason = computed(() => (quiet() ? "" : props.reason()));
  const wire = computed(() => card().wire?.value ?? "");
  const overhead = computed(() => card().wire?.overhead ?? "");
  const wireTip = computed(() => card().wire?.tip ?? "");
  const jitter = computed(() => (quiet() ? "" : (card().jitter ?? "")));
  const noData = computed(() => (quiet() ? "" : (card().noData ?? "")));
  const line = computed(() => (reason() ? "reason" : wire() ? "wire" : jitter() ? "jitter" : null));
  const strip = computed(() => props.strip?.() ?? null);
  const element = (
    <article
      class={() => `card panel ${status()}${strip() ? " with-strip" : ""}`}
      data-tone={stage}
      data-flip={`card-${stage}`}
      data-flip-resize=""
    >
      <span class="face">
        <span class="name">
          <span class="tone-icon" aria-hidden="true">
            <Icon name={STAGE[stage].icon} />
          </span>
          <h3>{STAGE[stage].label}</h3>
          <Show when={word}>
            {(shown) => (
              <span class="status">
                {() => {
                  const tone = shown().tone;
                  return tone && tone !== "neutral" && <span class="status-dot" data-tone={tone} />;
                }}
                {() => shown().label}
              </span>
            )}
          </Show>
        </span>
        {() => {
          props.lens();
          return untrack(headline);
        }}
      </span>
      <span class="line">
        {() => {
          const kind = line();
          if (kind === "reason")
            return (
              <span class="reason" ref={clip()}>
                {reason}
              </span>
            );
          if (kind === "wire")
            return (
              <span class="wire">
                <span class="label" ref={term(wireTip)}>
                  {CARDS.wire}
                </span>{" "}
                {wire} <span class="delta">{overhead}</span>
              </span>
            );
          if (kind === "jitter")
            return (
              <span class="wire">
                <span class="label" ref={term(() => TIP.jitter)}>
                  {CARDS.jitter}
                </span>{" "}
                {jitter}
              </span>
            );
          return null;
        }}
        <Show when={noData}>
          {() => (
            <span class="no-data">
              <span class="label" ref={term(() => TIP.noData)}>
                {CARDS.noData}
              </span>{" "}
              {noData}
            </span>
          )}
        </Show>
      </span>
      {() => strip() && <div class="strip">{strip()}</div>}
      <dl class={() => (unknown() ? "facts unknown" : "facts")}>{facts}</dl>
      <span class="sr-only">
        <span class="spoken">{spoken}</span>
      </span>
    </article>
  ) as HTMLElement;
  return { element, figures };
}

/** The cards in a grid of up to four across, pairing up and then stacking as the console narrows. */
function Summary(props: {
  stages: Read<readonly Stage[]>;
  card: (stage: Stage) => HTMLElement;
  issues?: Node;
}): Node {
  const grid = (
    <div class="result-cards" ref={tipGroup}>
      <For each={props.stages} by={(stage) => stage}>
        {(stage) => props.card(stage())}
      </For>
    </div>
  ) as HTMLElement;
  const summary = (
    <div class="result-summary">
      {grid}
      {props.issues}
    </div>
  ) as HTMLElement;
  effect(() => {
    const count = String(Math.min(4, props.stages().length));
    summary.style.setProperty("--cards", count);
    grid.dataset.cards = count;
  });
  return summary;
}

/** A saved result's cards as History shows them; `lens` "" shows every server together. */
export function ResultSummary(props: {
  result: () => RunResult;
  lens: Read<string>;
  units: () => RateUnits;
  showWire: () => boolean;
}): Node {
  const { lens } = props;
  const evidence = computed<Evidence>(() => {
    const result = props.result();
    return lens()
      ? serverEvidence(result.multiServer, lens())
      : { ...result, status: result.stages, idle: result.latencyByStage.latency };
  });
  const stages = computed(() => STAGES.filter((stage) => settled(evidence().status[stage])));
  const issues = computed(() => serverIssues(props.result().multiServer, lens()));
  const attributed = computed(() => props.result().multiServer.selection.length > 1 && !lens());
  const listed = computed(() => issues().filter((issue) => !issue.throughput.length));
  const card = (stage: Stage) =>
    Card({
      card: computed(() => {
        const status = evidence().status[stage];
        return settledCard(
          stage,
          settled(status) ? status : "failed",
          evidence(),
          props.units(),
          props.showWire(),
        );
      }),
      reason: computed(() => cardReason(issues(), stage, attributed())),
      lens,
    }).element;
  return (
    <Summary
      stages={stages}
      card={card}
      issues={
        <Show when={() => listed().length > 0}>
          {() => (
            <dl class={() => (attributed() ? "issues enter attributed" : "issues enter")}>
              {() =>
                listed().map((issue: Issue) => [
                  attributed() ? (
                    [<dt>{issue.server}</dt>, <dd>{issue.stages}</dd>]
                  ) : (
                    <dt>{issue.stages}</dt>
                  ),
                  <dd class="reason">{issue.reason}</dd>,
                ])
              }
            </dl>
          )}
        </Show>
      }
    />
  );
}

const RUNNING_STATES = new Set<StageState>(["warmup", "running", "recovering"]);

/** The live console's cards: settled stages from the results, the running one on the frame clock, the rest waiting. */
export function ResultCards(props: { app: App; live: Live; away: Read<boolean> }): Node {
  const { app, live } = props;
  const { run } = app;
  const units = live.rateUnits;
  const details = run.details;
  const { lens } = live;
  const evidence = computed<Evidence>(() =>
    shownEvidence({
      details: details(),
      lens: lens(),
      focus: live.focus(),
      run: { ...run.results(), status: run.statuses() },
      idle: live.summaries().latency ?? null,
    }),
  );
  // Lost latency probes are marked on the latency card's facts; nothing is added under the cards mid-run.
  const issues = computed(() =>
    details() ? serverIssues(details()!, lens()).filter((issue) => issue.throughput.length) : [],
  );
  const attributed = computed(() => (details()?.selection.length ?? 1) > 1 && !lens());
  const plan = computed(() => run.plan() ?? app.preferences.config());

  /** What a stage that has not settled shows, read from the run only while it runs or after it stopped. */
  const liveEvidence = (stage: Stage, state: StageState): LiveEvidence => {
    const running = RUNNING_STATES.has(state);
    const idle = live.summaries().latency ?? null;
    const none = { own: false, lost: false, spoken: null, held: null, idle, bytes: 0, shape: null };
    if (!running && state !== "stopped") return none;
    if (stage === "latency") {
      const replies = live.replies();
      return {
        ...none,
        own: running && run.phase() === "latency",
        lost: replies.lost,
        spoken: replies.rttMs,
        held: idle?.p50Ms ?? null,
      };
    }
    live.stageSamples();
    const samples = run.throughput;
    const start = live.stageSamples.peek()[stage as TransferStage]?.first ?? 0;
    const sample = run.live();
    const lastOf = (dir: "down" | "up") =>
      samples.findLast((each) => each.stage === stage && each.dir === dir)?.bytesPerSec ?? 0;
    return {
      ...none,
      own: running && live.rateStage() === stage,
      spoken:
        sample?.stage === stage ? (sample.down ?? 0) + (sample.bridgedUp ?? sample.up ?? 0) : null,
      held: lastOf("down") + lastOf("up") || null,
      bytes: run.stageBytes(),
      shape: shapeSoFar(samples, stage, start),
    };
  };
  const model = (stage: Stage) =>
    computed<CardModel>(() => {
      const status = evidence().status[stage];
      if (settled(status))
        return settledCard(stage, status, evidence(), units(), app.preferences.showWireEstimates());
      // A server shown alone that never ran the stage reads as skipped.
      const state: StageState = status === "not-run" ? "skipped" : live.states()[stage];
      return liveCard(stage, state, liveEvidence(stage, state), units());
    });

  // Strips share one rate and one latency scale, so heights compare.
  const ceiling = computed(() => live.scales().chartBytesPerSec);
  const latencyTop = computed(() =>
    replyTrackMs(live.buckets().map((bucket) => bucket.maxRttMs ?? bucket.medianRttMs)),
  );
  const baseline = computed(() => live.summaries().latency?.p50Ms ?? null);
  const rate = (bytesPerSec: number) => fmtRate(bytesPerSec, units());
  const strip = (stage: Stage): (() => Node | null) => {
    const state = () => live.states()[stage];
    // A stage spans its plan until it settles, then the time it took.
    const waiting = () => RUNNING_STATES.has(state()) || state() === "upcoming";
    const ownBuckets = computed(() => live.bucketKeys()[stage] ?? 0);
    if (stage === "latency") {
      const points = computed(() => {
        ownBuckets();
        const replies = untrack(live.buckets).filter(
          (bucket) => bucket.phase === "latency" && bucket.medianRttMs !== null,
        );
        return { start: replies[0]?.startT ?? 0, end: replies.at(-1)?.endT ?? 0 };
      });
      const start = computed(() => points().start);
      const graph = StageGraph({
        stage,
        dirs: [],
        samples: () => run.throughput,
        own: () => 0,
        buckets: live.buckets,
        ownBuckets,
        start,
        span: computed(
          () => Math.max(points().end - start(), waiting() ? plan().duration.latencyMs : 0) || 1,
        ),
        ceiling,
        latencyTop,
        baseline,
        rate,
        live: () => false,
        head: () => false,
        active: live.active,
        beat: live.beat,
        label: CARDS.idleOverTime,
      });
      return () => graph;
    }
    const transfer = stage as TransferStage;
    const dirs =
      transfer === "bidirectional"
        ? (["down", "up"] as const)
        : transfer === "download"
          ? (["down"] as const)
          : (["up"] as const);
    const own = computed(() => {
      const series = live.stageSamples()[transfer];
      return series ? `${series.count}:${series.first}:${series.last}` : "";
    });
    const start = computed(() => (own(), live.stageSamples.peek()[transfer]?.first ?? 0));
    const span = computed(() => {
      own();
      const series = live.stageSamples.peek()[transfer];
      const measured = series ? series.last - series.first : 0;
      return Math.max(measured, waiting() ? plan().duration[`${transfer}Ms`] : 0) || 1;
    });
    const running = computed(() => RUNNING_STATES.has(state()) && live.rateStage() === transfer);
    const head = (now: number, into: Head) => {
      into.t = run.shownAt.peek() + live.phaseClock.at(now);
      dirs.forEach((dir, index) => {
        into.values[index] = (dir === "down" ? live.down : live.up).at(now);
      });
      return true;
    };
    const graph = StageGraph({
      stage,
      dirs,
      samples: () => run.throughput,
      own,
      buckets: live.buckets,
      ownBuckets,
      start,
      span,
      ceiling,
      latencyTop,
      baseline,
      rate,
      live: running,
      head,
      active: live.active,
      beat: live.beat,
      label: CARDS.overTime(STAGE[stage].label),
    });
    // A server shown alone has no series of its own.
    return () => (lens() ? null : graph);
  };

  const card = (stage: Stage) => {
    const view = model(stage);
    const { element, figures } = Card({
      card: view,
      reason: computed(() => cardReason(issues(), stage, attributed())),
      lens,
      strip: strip(stage),
    });
    // The running card's figures follow the frame clock; a figure is written only when what it shows changes.
    const glided = computed(() => view().live);
    let first = Number.NaN;
    let second = Number.NaN;
    const draw = (now: number) => {
      const shown = glided.peek();
      if (!shown) return false;
      const page = units.peek();
      const tier = page.tier ?? 0;
      if (shown === "rtt") {
        const ms = live.rtt.at(now);
        if (Math.round(ms * 10) !== first) {
          first = Math.round(ms * 10);
          figures.num.set(fmtMs(ms));
        }
        return live.active(now);
      }
      const down = toRateUnit(live.down.at(now), page, tier);
      const up = toRateUnit(live.up.at(now), page, tier);
      const a = Math.round((shown === "rate" ? down + up : down) * 100);
      const b = shown === "rate" ? 0 : Math.round(up * 100);
      if (a !== first || b !== second) {
        first = a;
        second = b;
        if (shown === "rate") figures.num.set(fmtSpeed(down + up));
        else {
          figures.down.set(fmtSpeed(down));
          figures.up.set(fmtSpeed(up));
          figures.combined?.set(`${fmtSpeed(down + up)} ${rateUnit(page, tier)}`);
        }
      }
      return live.active(now);
    };
    const wake = painter(draw, props.away, live.beat);
    effect(() => {
      if (!glided()) return;
      units();
      run.running();
      first = Number.NaN;
      wake();
    });
    return element;
  };

  // Once per completed run, never again for a unit or lens change; a result already shown at mount is not news.
  let spoken = run.result.peek();
  effect(() => {
    const result = run.result();
    if (run.phase() !== "complete" || !result || result === spoken) return;
    spoken = result;
    const cards = STAGES.filter((stage) => settled(result.stages[stage])).map((stage) =>
      settledCard(
        stage,
        result.stages[stage] as "complete",
        { ...result, status: result.stages, idle: result.latencyByStage.latency },
        untrack(units),
        false,
      ),
    );
    untrack(() => announce(resultSentence(cards)));
  });

  return (
    <Summary
      stages={computed(() => STAGES.filter((stage) => live.visibility(stage).card))}
      card={card}
    />
  );
}
