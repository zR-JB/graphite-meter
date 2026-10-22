// The latency lanes: a row per probe population, its figures and its spread as a box plot on one shared axis.
import type { App } from "../../app/app";
import type { AddedLatency, LatencySummary } from "../../engine/measure/latency";
import { STAGES, type Stage } from "../../engine/run/schedule";
import { For, Show } from "../../lib/dom";
import { Glide, handoff, reducedMotion } from "../../lib/motion";
import { computed, effect, signal, type Read } from "../../lib/signal";
import { fmtAddedMs, fmtLatency, fmtMs, fmtTick, MISSING } from "../../view/format";
import { POPULATION, reasonLabel, STAGE, STAGE_STATE, TIP } from "../../view/words";
import { announceChanges } from "../kit/announce";
import { Icon } from "../kit/icon";
import { tipGroup } from "../kit/rove";
import { ServerScope } from "../kit/scope";
import { clip, term, tip, warmTips } from "../kit/tip";
import { LANES } from "./copy";
import {
  accountingNotice,
  fmtTimeouts,
  laneDescription,
  lanesAxisMs,
  metricLabel,
  metricMeaning,
  nearestMetric,
  present,
  serverHandling,
  stepMetric,
  TIMEOUTS_WARN,
  timeoutShare,
  timeoutsTip,
  values,
  type Metric,
  type Values,
} from "./lanes-model";
import type { Live } from "./live";
import { inView, measure, painter, scrub } from "./paint";

export interface LaneInput {
  stage: Stage;
  summary: LatencySummary | null;
  /** The newest reply's median while the stage runs; the live view marks it. */
  current?: number | null;
  /** The stage runs now, its warmup included: the row washes in. */
  active?: boolean;
  failure?: string;
}

/** What stands beside the title: why the idle stage failed, a choice of whose latency to show, or whose it is. */
type LanesHead =
  | { kind: "none" }
  | { kind: "failure"; text: string }
  | { kind: "picker"; node: Node }
  | { kind: "source"; text: string };

/** Marks keep this margin, so one at either end of the axis stays whole. */
const EDGE = 7;
const GLIDED = ["min", "max", "p10", "p90", "center", "current"] as const;
const NONE: LanesHead = { kind: "none" };

interface Row {
  lane: LaneInput;
  values: Values;
}

/** The lanes of a live run ("bare": marks glide, the latest reply is marked) or a saved result ("compact"), in stage order. */
export function LatencyProfileView(props: {
  lanes: () => readonly LaneInput[];
  variant?: "bare" | "compact";
  label?: string;
  added?: () => AddedLatency | null;
  head?: () => LanesHead;
  beat?: () => number | null;
  /** While it holds the marks draw nothing, and they draw again as the lanes return. */
  away?: Read<boolean>;
}): HTMLElement {
  const live = props.variant !== "compact";
  const made = new WeakMap<LaneInput, Row>();
  const rows = computed(() =>
    props.lanes().map((lane) => {
      let row = made.get(lane);
      if (!row)
        made.set(lane, (row = { lane, values: values(lane.summary, lane.current ?? null) }));
      return row;
    }),
  );
  const idleCenter = computed(
    () => rows().find((row) => row.lane.stage === "latency")?.values.center ?? null,
  );
  const idleActive = computed(
    () => !!rows().find((row) => row.lane.stage === "latency")?.lane.active,
  );
  const measured = computed(() => rows().some((row) => row.values.center !== null));
  const several = computed(() => rows().length > 1);
  const axis = computed(() => lanesAxisMs(rows().map((row) => row.values)));
  // Lit like a stage card: dim until something is measured, brightest while the idle stage runs.
  const light = computed(() => (idleActive() ? "active" : measured() ? "complete" : "pending"));
  const ticks = handoff(
    () => [0, axis() / 2, axis()].map((tick) => fmtTick(tick)),
    (texts) => texts.join(),
  );
  const head = computed(() => props.head?.() ?? NONE);
  const lanes = (
    <div
      class="lanes"
      data-variant={live ? "bare" : "compact"}
      role="group"
      aria-label={LANES.group}
      ref={tipGroup}
    />
  ) as HTMLElement;
  const seen = live ? inView(lanes, false) : () => false;
  const motion = computed(() => seen() && !reducedMotion());
  // A reading follows the pointer or the keyboard, on one row at a time.
  const reading = signal<{ stage: Stage; metric: Metric } | null>(null);
  let keyboard: Stage | null = null;
  lanes.style.setProperty("--edge", `${EDGE}px`);
  effect(() => lanes.style.setProperty("--lanes", String(rows().length)));

  lanes.append(
    <div class="lane-head" aria-hidden="true">
      {LANES.heads.map((text) => (
        <span>{text}</span>
      ))}
    </div>,
    <For each={rows} by={(row) => row.lane.stage}>
      {(row) => (
        <Lane
          row={row}
          live={live}
          away={props.away}
          beat={props.beat}
          motion={motion}
          axis={axis}
          idle={idleCenter}
          several={several}
          added={computed(() => {
            const { lane, values } = row();
            if (lane.stage === "latency") return null;
            const saved = props.added?.()?.[lane.stage];
            const base = idleCenter();
            return saved ?? (values.center === null || base === null ? null : values.center - base);
          })}
          reading={reading}
          keyboard={{
            get: () => keyboard,
            set: (stage) => {
              keyboard = stage;
            },
          }}
        />
      )}
    </For>,
    <div
      class={() => (ticks.out() ? "ticks handoff handoff-out" : "ticks handoff")}
      aria-hidden="true"
    >
      {[0, 1, 2].map((index) => {
        const tick = (
          <span>{() => `${ticks.shown()[index] ?? ""}${index === 2 ? " ms" : ""}`}</span>
        ) as HTMLElement;
        tick.style.left = `${index * 50}%`;
        return tick;
      })}
    </div>,
  );

  return (
    <section
      class={() => `latency-card stage-area ${light()}`}
      data-tone="latency"
      aria-label={props.label ?? LANES.label}
    >
      <header class="card-head">
        <span class="tone-icon" aria-hidden="true">
          <Icon name={STAGE.latency.icon} />
        </span>
        <h3 ref={tip(() => TIP.latency)}>{STAGE.latency.label}</h3>
        {() => {
          const shown = head();
          if (shown.kind === "failure" && idleCenter() === null)
            return (
              <span class="aside failure" ref={clip()}>
                {shown.text}
              </span>
            );
          if (shown.kind === "picker") return <span class="source">{shown.node}</span>;
          if (shown.kind === "source")
            return (
              <span class="aside" ref={clip()}>
                {shown.text}
              </span>
            );
          return null;
        }}
      </header>
      <div class="body">{lanes}</div>
    </section>
  ) as HTMLElement;
}

function Lane(props: {
  row: Read<Row>;
  live: boolean;
  away?: Read<boolean>;
  beat?: () => number | null;
  motion: () => boolean;
  axis: () => number;
  idle: () => number | null;
  several: () => boolean;
  added: Read<number | null>;
  reading: ReturnType<typeof signal<{ stage: Stage; metric: Metric } | null>>;
  keyboard: { get(): Stage | null; set(stage: Stage | null): void };
}): Node {
  const { row, reading } = props;
  const stage = row.peek().lane.stage;
  const label = POPULATION[stage];
  const lane = computed(() => row().lane);
  const shown = computed(() => row().values);
  const summary = computed(() => lane().summary);
  const plus = props.added;
  const metrics = computed(() => present(shown()));
  const selected = computed(() => {
    const now = reading();
    return now?.stage === stage && shown()[now.metric] !== null ? now.metric : null;
  });
  // Focus reads the median first; the slider rests there between readings.
  const resting = computed(
    () => selected() ?? (shown().center !== null ? "center" : (metrics()[0] ?? null)),
  );
  const pct = (ms: number) => Math.min(100, Math.max(0, (ms / props.axis()) * 100));
  const overflow = computed(() => (shown().max ?? 0) > props.axis());

  // The row's plot, sized by its attributes from the track's measured box.
  const track = (
    <div
      class="track"
      role="slider"
      data-lane={stage}
      tabindex={() => (metrics().length ? 0 : -1)}
      aria-label={computed(() => laneDescription(label, shown(), summary(), plus()))}
      aria-disabled={() => (metrics().length ? undefined : "true")}
      aria-valuemin="0"
      aria-valuemax={() => Math.max(0, metrics().length - 1)}
      aria-valuenow={() => {
        const at = resting();
        return at ? metrics().indexOf(at) : 0;
      }}
      aria-valuetext={() => {
        const metric = resting();
        return metric
          ? `${metricLabel(metric)} ${fmtMs(shown()[metric])} ms, ${metricMeaning(metric)}`
          : undefined;
      }}
      {...scrub(
        (event) => {
          const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
          const ratio = Math.min(
            1,
            Math.max(0, (event.clientX - box.left - EDGE) / (box.width - 2 * EDGE)),
          );
          const metric = nearestMetric(shown(), ratio * props.axis());
          if (metric) reading.set({ stage, metric });
          else if (props.keyboard.get() !== stage) reading.set(null);
        },
        () => {
          if (props.keyboard.get() !== stage) reading.set(null);
        },
      )}
      onpointerleave={(event) => {
        if (event.pointerType === "touch") return;
        if (reading.peek()) warmTips();
        if (props.keyboard.get() !== stage) reading.set(null);
      }}
      onfocus={(event) => {
        if (!event.currentTarget.matches(":focus-visible")) return;
        props.keyboard.set(stage);
        const first = resting();
        if (first) reading.set({ stage, metric: first });
      }}
      onblur={() => {
        props.keyboard.set(null);
        reading.set(null);
      }}
      onkeydown={(event) => {
        if (event.key === "Escape") {
          if (selected()) event.preventDefault();
          reading.set(null);
          return;
        }
        const metric = stepMetric(shown(), selected(), event.key);
        if (!metric) return;
        event.preventDefault();
        reading.set({ stage, metric });
      }}
    />
  ) as HTMLElement;
  const size = measure({ track }).track;
  // Marks placed by percentages of the axis, so a resize moves them in layout; they paint in this order.
  const art = (<span class="lane-art" aria-hidden="true" />) as HTMLElement;
  const mark = (name: string) => Object.assign(document.createElement("i"), { className: name });
  const range = mark("range");
  const reach = mark("overflow");
  const band = mark("band");
  const span = mark("added-span");
  const center = mark("center-marker");
  const current = mark("current-marker");
  const order = [range, reach, band, span, center, current];
  track.append(
    art,
    <Show when={selected}>
      {(metric) => <Reading metric={metric} values={shown} pct={pct} width={() => size().width} />}
    </Show>,
  );
  // A whisker past the axis runs on to the track's end, points there and names its value.
  effect(() => {
    reach.dataset.max = `${fmtMs(shown().max)} ms`;
  });

  // Marks glide to each summary on the frame clock while seen; a saved or unseen row snaps.
  const glides = new Map<(typeof GLIDED)[number], Glide>();
  const wake = painter((now) => draw(now), props.away, props.beat);
  effect(() => {
    const values = shown();
    const snap = !props.motion();
    for (const metric of GLIDED) {
      const value = values[metric];
      let glide = glides.get(metric);
      if (value === null) {
        glides.delete(metric);
        continue;
      }
      const target = pct(value);
      if (!glide) glides.set(metric, (glide = new Glide(target)));
      if (snap || glide.target !== target) glide.set(target, { snap });
    }
    wake();
  });
  const attach = (node: HTMLElement, on: boolean) => {
    if (on === node.isConnected) return;
    if (!on) return node.remove();
    const later = order.slice(order.indexOf(node) + 1).find((each) => each.isConnected);
    art.insertBefore(node, later ?? null);
  };
  // What each mark was last given, so a frame writes only what moved.
  const placed = new WeakMap<HTMLElement, string>();
  const place = (node: HTMLElement, left: number, width?: number) => {
    const at = width === undefined ? `${left}` : `${left} ${width}`;
    if (placed.get(node) === at) return;
    placed.set(node, at);
    node.style.left = `${left}%`;
    if (width !== undefined) node.style.width = `${Math.max(0, width)}%`;
  };
  function draw(now: number): boolean {
    const at = (metric: (typeof GLIDED)[number]) => glides.get(metric)?.at(now) ?? null;
    let moving = false;
    for (const glide of glides.values()) moving ||= glide.at(now) !== glide.target;
    const values = shown.peek();
    const min = at("min");
    const max = at("max");
    const whisker = min !== null && max !== null;
    attach(range, whisker);
    if (whisker) place(range, min, max - min);
    const over = whisker && overflow.peek();
    attach(reach, over);
    if (over) place(reach, max);
    const p10 = at("p10");
    const p90 = at("p90");
    const box = p10 !== null && p90 !== null;
    attach(band, box);
    if (box) place(band, p10, p90 - p10);
    // Under the box, clear of the whisker: a bracket from the idle median to a loaded median, what the load added.
    const base = props.idle();
    const middle = at("center");
    const from = base !== null && props.several() && (values.center ?? 0) > base ? pct(base) : null;
    attach(span, from !== null && middle !== null);
    if (from !== null && middle !== null) place(span, from, middle - from);
    attach(center, middle !== null);
    if (middle !== null) place(center, middle);
    const latest = props.live ? at("current") : null;
    attach(current, latest !== null);
    if (latest !== null) place(current, latest);
    return moving;
  }
  effect(() => {
    props.idle();
    props.several();
    overflow();
    wake();
  });

  const median = computed(() => fmtLatency(shown().center));
  const jitter = computed(() => fmtLatency(summary()?.jitterMs));
  const noted = computed(() => accountingNotice(summary()));
  const ratio = computed(() => timeoutShare(summary()));
  const timeouts = computed(() => fmtTimeouts(ratio()));
  const timeoutsHelp = computed(() => timeoutsTip(label, summary()));
  const addedText = computed(() =>
    stage === "latency" ? "" : plus() === null ? MISSING : `${fmtAddedMs(plus())} ms`,
  );
  return (
    <div
      class="lane"
      data-flip={`lane-${stage}`}
      data-tone={stage}
      data-active={() => (lane().active ? "true" : "false")}
    >
      <span class="lane-name">
        <span class="tone-icon" aria-hidden="true">
          <Icon name={STAGE[stage].icon} />
        </span>
        <span
          class={() => (lane().failure ? "lane-label failed" : "lane-label")}
          ref={tip(() => (lane().failure ? `${label}\n${lane().failure}` : ""))}
        >
          {label}
        </span>
      </span>
      <strong
        class={() => (shown().center === null ? "lane-median quiet" : "lane-median")}
        ref={tip(() => {
          const timing = summary()?.reflectorTiming;
          return timing ? `${TIP.latencyMedian}\n${serverHandling(timing)}` : TIP.latencyMedian;
        })}
      >
        {median}
      </strong>
      <em
        class={() => (summary()?.jitterMs == null ? "lane-jitter quiet" : "lane-jitter")}
        ref={tip(() => TIP.jitter)}
      >
        {jitter}
      </em>
      {/* Missing replies brighten the figure and mark it as a term that explains them; only many of them warn. */}
      <Show
        when={noted}
        fallback={() => (
          <em
            class={() => (ratio() === null ? "lane-timeouts quiet" : "lane-timeouts")}
            ref={tip(timeoutsHelp)}
          >
            {timeouts}
          </em>
        )}
      >
        {() => (
          <em
            class={() =>
              (ratio() ?? 0) >= TIMEOUTS_WARN ? "lane-timeouts noted many" : "lane-timeouts noted"
            }
            ref={term(timeoutsHelp)}
          >
            {timeouts}
          </em>
        )}
      </Show>
      {track}
      <span
        class={() => (plus() === null ? "lane-added quiet" : "lane-added")}
        ref={tip(() => (stage === "latency" ? "" : TIP.addedLatency))}
      >
        {addedText}
      </span>
    </div>
  );
}

/** The plate reading the mark nearest the pointer, as far along its width as the mark lies along the plot. */
function Reading(props: {
  metric: Read<Metric>;
  values: Read<Values>;
  pct: (ms: number) => number;
  width: () => number;
}): Node {
  const value = computed(() => props.values()[props.metric()]);
  const along = computed(() => props.pct(value() ?? 0));
  const guide = (<span class="guide" />) as HTMLElement;
  const plate = (
    <span class="inspect-card hover-card">
      <span>
        <small>{() => metricLabel(props.metric())}</small> {() => `${fmtMs(value())} ms`}
      </span>
    </span>
  ) as HTMLElement;
  effect(() => {
    const x = EDGE + (along() / 100) * (props.width() - 2 * EDGE);
    guide.style.translate = `${x}px 0`;
    plate.style.translate = `calc(${x}px - ${along()}%) 0`;
  });
  return (
    <>
      {guide}
      {plate}
    </>
  );
}

/** The live console's lanes, whose server to show when several run, and the idle stage's failure, heard once. */
export function LatencyProfile(props: { app: App; live: Live; away?: Read<boolean> }): Node {
  const { app, live } = props;
  const { run } = app;
  const servers = live.indicated;
  // Lost probes name their reason on their population's row; the lanes show one server, so only its failures count.
  const notes = computed(() => {
    const notes: Partial<Record<Stage, string>> = {};
    const focus = live.focus();
    for (const failure of run.details()?.failures ?? [])
      if (failure.scope === "latency" && failure.serverId === focus)
        notes[failure.stage] = [notes[failure.stage], reasonLabel(failure.reason)]
          .filter(Boolean)
          .join("\n");
    return notes;
  });
  const newest = (stage: Stage) =>
    live.buckets().findLast((bucket) => bucket.phase === stage && bucket.medianRttMs !== null)
      ?.medianRttMs ?? null;
  const kept = new Map<Stage, LaneInput>();
  // The same lanes are the same array, so a reply that changes no lane redraws nothing downstream.
  let shown: LaneInput[] = [];
  const lanes = computed<LaneInput[]>(() => {
    const next = STAGES.filter((stage) => live.visibility(stage).lane).map((stage) => {
      const state = live.states()[stage];
      const active = state === "warmup" || state === "running" || state === "recovering";
      // A settled lane draws like a saved one: the latest reply marks only a running stage.
      const next: LaneInput = {
        stage,
        summary: live.summaries()[stage] ?? null,
        current: active ? newest(stage) : null,
        active,
        failure: notes()[stage],
      };
      const previous = kept.get(stage);
      if (
        previous &&
        previous.summary === next.summary &&
        previous.current === next.current &&
        previous.active === next.active &&
        previous.failure === next.failure
      )
        return previous;
      kept.set(stage, next);
      return next;
    });
    return next.length === shown.length && next.every((lane, i) => lane === shown[i])
      ? shown
      : (shown = next);
  });
  const failure = computed(() => {
    if (live.states().latency !== "failed") return null;
    const found = run
      .details()
      ?.failures.find((each) => each.stage === "latency" && each.scope === "latency");
    return found ? reasonLabel(found.reason) : STAGE_STATE.failed.label;
  });
  announceChanges(() => {
    if (live.states().latency !== "failed") return "";
    const reason = failure();
    return `${STAGE.latency.label} ${STAGE_STATE.failed.label.toLowerCase()}${reason === STAGE_STATE.failed.label ? "" : `: ${reason}`}`;
  });
  // Every server is probed; the lanes show one at a time, chosen here at any moment, mid-run included.
  const picker: LanesHead = {
    kind: "picker",
    node: (
      <ServerScope
        quiet
        label={LANES.picker}
        servers={servers}
        value={() => {
          const focus = live.focus();
          return servers().some(({ id }) => id === focus)
            ? (focus ?? "")
            : (app.servers.latencyServer() ?? "");
        }}
        onChange={live.showLatency}
      />
    ),
  };
  const several = computed(() => servers().length > 1);
  const failed = computed<LanesHead | null>(() => {
    const reason = failure();
    return reason ? { kind: "failure", text: reason } : null;
  });
  const head = computed<LanesHead>(() => failed() ?? (several() ? picker : NONE));
  const added = computed(() => {
    if (!run.result()) return null;
    return (
      run.details()?.servers.find((entry) => entry.server.id === live.focus())?.addedLatency ?? null
    );
  });
  return (
    <div class="live-profile">
      <LatencyProfileView
        lanes={lanes}
        variant="bare"
        added={added}
        head={head}
        away={props.away}
        beat={live.beat}
      />
    </div>
  );
}
