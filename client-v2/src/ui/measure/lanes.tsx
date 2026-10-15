// The latency lanes: one ruled row per stage's probe population, its median, jitter, timeouts and added latency in
// figures and its spread as a box plot on one shared axis. Each row's plot is marks placed by percentages of the
// axis, so a resize moves them in layout alone; a mark that glides between summaries rewrites its own place and
// nothing else.
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

/** One population's row. */
export interface LaneInput {
  stage: Stage;
  summary: LatencySummary | null;
  /** The newest reply's median while the stage runs; the live view marks it. */
  current?: number | null;
  /** The stage runs now, its warmup included: the row washes in. */
  active?: boolean;
  /** Why this population's probes stopped. */
  failure?: string;
}

/** What stands beside the title: why the idle stage failed, a choice of whose latency to show, or whose it is. */
export type LanesHead =
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

/** The lanes for a live run ("bare": marks glide, the latest reply is marked) or a saved result ("compact"), rows in
    stage order. A row kept as the same object draws nothing again; until the run has `added`, the medians give it. */
export function LatencyProfileView(props: {
  lanes: () => readonly LaneInput[];
  variant?: "bare" | "compact";
  label?: string;
  added?: () => AddedLatency | null;
  head?: () => LanesHead;
  /** While it holds the marks draw nothing, and they draw again as the lanes return. */
  away?: Read<boolean>;
}): HTMLElement {
  const live = props.variant !== "compact";
  const made = new WeakMap<LaneInput, Row>();
  const rows = computed(() =>
    props.lanes().map((lane) => {
      let row = made.get(lane);
      if (!row) made.set(lane, (row = { lane, values: values(lane.summary, lane.current ?? null) }));
      return row;
    }),
  );
  const idleCenter = computed(() => rows().find((row) => row.lane.stage === "latency")?.values.center ?? null);
  const idleActive = computed(() => !!rows().find((row) => row.lane.stage === "latency")?.lane.active);
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
      data-latency-profile=""
      data-variant={live ? "bare" : "compact"}
      role="group"
      aria-label={LANES.group}
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
    <div class={() => (ticks.out() ? "ticks handoff handoff-out" : "ticks handoff")} aria-hidden="true">
      {[0, 1, 2].map((index) => {
        const tick = (<span>{() => `${ticks.shown()[index] ?? ""}${index === 2 ? " ms" : ""}`}</span>) as HTMLElement;
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
      aria-valuenow={() => Math.max(0, selected() ? metrics().indexOf(selected()!) : 0)}
      aria-valuetext={() => {
        const metric = selected();
        const value = metric && shown()[metric];
        return metric && value != null
          ? `${metricLabel(metric)} ${fmtMs(value)} milliseconds, ${metricMeaning(metric)}`
          : undefined;
      }}
      {...scrub(
        (event) => {
          const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
          const ratio = Math.min(1, Math.max(0, (event.clientX - box.left - EDGE) / (box.width - 2 * EDGE)));
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
        const first = shown().center !== null ? "center" : (metrics()[0] ?? null);
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
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key) || !metrics().length) return;
        event.preventDefault();
        const metric = stepMetric(shown(), selected(), event.key);
        if (metric) reading.set({ stage, metric });
      }}
    />
  ) as HTMLElement;
  const size = measure({ track }).track;
  // The row's plot as marks placed by percentages of the axis, as v1 drew it: a resize moves them in layout with no
  // script, and a glide rewrites only its marks' own places. Marks paint in this order; each is in the plot while it
  // has a value.
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

  // Marks glide to each summary on the frame clock while the lanes are seen; a saved or unseen row snaps. A glide is
  // made on a metric's first value and let go when the value goes.
  const glides = new Map<(typeof GLIDED)[number], Glide>();
  const wake = painter((now) => draw(now), props.away);
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
  const present$ = (node: HTMLElement, on: boolean) => {
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
    present$(range, whisker);
    if (whisker) place(range, min, max - min);
    const over = whisker && overflow.peek();
    present$(reach, over);
    if (over) place(reach, max);
    const p10 = at("p10");
    const p90 = at("p90");
    const box = p10 !== null && p90 !== null;
    present$(band, box);
    if (box) place(band, p10, p90 - p10);
    // Under the box, clear of the whisker: a bracket from the idle median to a loaded median, what the load added.
    const base = props.idle();
    const centre = at("center");
    const from = base !== null && props.several() && (values.center ?? 0) > base ? pct(base) : null;
    present$(span, from !== null && centre !== null);
    if (from !== null && centre !== null) place(span, from, centre - from);
    present$(center, centre !== null);
    if (centre !== null) place(center, centre);
    const latest = props.live ? at("current") : null;
    present$(current, latest !== null);
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
    stage === "latency" ? LANES.baseline : plus() === null ? MISSING : `${fmtAddedMs(plus())} ms`,
  );
  return (
    <div
      class="lane"
      data-flip={`lane-${stage}`}
      data-tone={stage}
      data-active={() => (lane().active ? "true" : "false")}
    >
      <span class="lane-name">
        <span class="mark tone-icon" aria-hidden="true">
          <Icon name={STAGE[stage].icon} />
        </span>
        <span
          class={() => (lane().failure ? "lane-label failed" : "lane-label")}
          tabindex="-1"
          ref={tip(() => (lane().failure ? `${label}\n${lane().failure}` : ""))}
        >
          {label}
        </span>
      </span>
      <strong
        class={() => (shown().center === null ? "lane-median quiet" : "lane-median")}
        tabindex="-1"
        ref={tip(() => {
          const timing = summary()?.reflectorTiming;
          return timing ? `${TIP.latencyMedian}\n${serverHandling(timing)}` : TIP.latencyMedian;
        })}
      >
        {median}
      </strong>
      <em
        class={() => (summary()?.jitterMs == null ? "lane-jitter quiet" : "lane-jitter")}
        tabindex="-1"
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
            tabindex="-1"
            ref={tip(timeoutsHelp)}
          >
            {timeouts}
          </em>
        )}
      >
        {() => (
          <em
            class={() => ((ratio() ?? 0) >= TIMEOUTS_WARN ? "lane-timeouts noted many" : "lane-timeouts noted")}
            tabindex="-1"
            ref={term(timeoutsHelp)}
          >
            {timeouts}
          </em>
        )}
      </Show>
      {track}
      <span
        class={() => (plus() === null ? "lane-added quiet" : "lane-added")}
        tabindex="-1"
        ref={tip(() => TIP.addedLatency)}
      >
        {addedText}
      </span>
    </div>
  );
}

/** The plate reading the row: the figure of the mark nearest the pointer, over it and inside the plot, and a guide on
    the mark. The plate stands as far along its own width as the mark lies along the plot, so it never leaves the
    plot and is never measured; both are placed before they show, then glide from mark to mark. */
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

/** The live console's lanes: the run's rows, whose latency to show when several servers run, and the idle stage's
    failure, heard once when it fails. A row stays the same object while its values do, so it draws nothing again. */
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
        notes[failure.stage] = [notes[failure.stage], reasonLabel(failure.reason)].filter(Boolean).join("\n");
    return notes;
  });
  const newest = (stage: Stage) =>
    live.buckets().findLast((bucket) => bucket.phase === stage && bucket.medianRttMs !== null)?.medianRttMs ?? null;
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
    return next.length === shown.length && next.every((lane, i) => lane === shown[i]) ? shown : (shown = next);
  });
  const failure = computed(() => {
    if (live.states().latency !== "failed") return null;
    const found = run.details()?.failures.find((each) => each.stage === "latency" && each.scope === "latency");
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
          return servers().some(({ id }) => id === focus) ? (focus ?? "") : (app.servers.latencyServer() ?? "");
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
    return run.details()?.servers.find((entry) => entry.server.id === live.focus())?.addedLatency ?? null;
  });
  return (
    <div class="live-profile">
      <LatencyProfileView lanes={lanes} variant="bare" added={added} head={head} away={props.away} />
    </div>
  );
}
