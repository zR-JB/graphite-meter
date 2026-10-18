// A card's strip plots the stage's rate over its reply track, one bar per closed bucket, read by pointer or keys.
import type { ThroughputSample } from "../../app/series";
import type { Direction } from "../../engine/types";
import type { LatencyBucket } from "../../engine/measure/buckets";
import type { Stage, TransferStage } from "../../engine/run/schedule";
import { computed, effect, signal } from "../../lib/signal";
import { fmtDuration, fmtLatency, fmtLatencyRange } from "../../view/format";
import { STAGE } from "../../view/words";
import { warmTips } from "../kit/tip";
import { CARDS } from "./copy";
import { inView, measure, painter, scrub, setAttr, svg } from "./paint";
import {
  columnsFor,
  Lane,
  monotoneTangents,
  nearestIndex,
  replyBars,
  type ReplyBar,
} from "./strip-model";

const TOP_PAD = 3;
/** A readout keeps this gap from the cursor beside it. */
const GAP = 10;

/** The running stage's leading edge at a frame holds now on the timeline and each lane's glided rate (bytes/s). */
export interface Head {
  t: number;
  values: number[];
}

interface StripProps {
  stage: Stage;
  /** Lanes go by direction, one for download or upload, two for bidirectional, none for the latency card. */
  dirs: readonly Direction[];
  samples: () => readonly ThroughputSample[];
  /** Changes only when this stage's samples do. */
  own: () => unknown;
  buckets: () => readonly LatencyBucket[];
  /** Changes only when this stage's buckets do. */
  ownBuckets: () => unknown;
  start: () => number;
  span: () => number;
  /** Every strip shares the bytes/s at the plot's top and the ms at the track's, so heights compare. */
  ceiling: () => number;
  latencyTop: () => number;
  /** The idle median is drawn across the track. */
  baseline: () => number | null;
  rate: (bytesPerSec: number) => string;
  /** The stage runs on its own rates, so its leading edge moves every frame. */
  live: () => boolean;
  /** Writes the leading edge at `now` into `head`; false when there is none. */
  head: (now: number, head: Head) => boolean;
  active: (now: number) => boolean;
  label: string;
}

interface Row {
  label: string;
  value: string;
  mark?: string;
  /** The readout keys the row to the first lane's line, the second's, or a reply bar. */
  key: "line" | "second" | "reply";
}

let strips = 0;
const STEP: Partial<Record<string, number>> = {
  ArrowLeft: -1,
  ArrowDown: -1,
  ArrowRight: 1,
  ArrowUp: 1,
};

const shown = (mark: SVGElement, visible: boolean) =>
  setAttr(mark, "visibility", visible ? "visible" : "hidden");
const fixed = (value: number) => (Math.round(value * 100) / 100).toString();

export function StageGraph(props: StripProps): HTMLElement {
  const { dirs } = props;
  const trackOnly = dirs.length === 0;
  const phase = props.stage as LatencyBucket["phase"];

  // The track holds the idle median's dashed line, one bar per joined bucket and the cursor.
  const track = (<div class={trackOnly ? "track full" : "track"} />) as HTMLElement;
  const trackArt = svg("svg", "art");
  trackArt.setAttribute("aria-hidden", "true");
  const median = svg("line", "reply-median");
  const trackCursor = svg("line", "cursor");
  trackArt.append(median, trackCursor);
  track.append(trackArt);
  shown(median, false);
  shown(trackCursor, false);

  // The plot draws lane 0 over a wash of its hue fading down from the line, the second lane dashed, and the head dots.
  const plot = (<div class="plot" />) as HTMLElement;
  const plotArt = svg("svg", "art");
  plotArt.setAttribute("aria-hidden", "true");
  const fill = `strip-${++strips}-fill`;
  const gradient = svg("linearGradient");
  gradient.id = fill;
  for (const [key, value] of [
    ["x1", "0"],
    ["y1", "0"],
    ["x2", "0"],
    ["y2", "1"],
  ])
    gradient.setAttribute(key!, value!);
  for (const [offset, opacity] of [
    ["0", "0.2"],
    ["1", "0"],
  ]) {
    const stop = svg("stop");
    stop.setAttribute("offset", offset!);
    stop.setAttribute("stop-opacity", opacity!);
    gradient.append(stop);
  }
  const defs = svg("defs");
  defs.append(gradient);
  const axis = svg("line", "axis");
  const area = svg("path", "area");
  area.setAttribute("fill", `url(#${fill})`);
  const lines = dirs.map((_, index) => svg("path", index ? "line second" : "line"));
  const heads = dirs.map(() => svg("circle", "head"));
  for (const head of heads) {
    head.setAttribute("r", "3.5");
    shown(head, false);
  }
  const plotCursor = svg("line", "cursor");
  shown(plotCursor, false);
  plotArt.append(defs, axis, area, ...lines, ...heads, plotCursor);
  plot.append(plotArt);

  const sizes = measure({ plot, track });
  const plotWidth = computed(() => sizes.plot().width);
  const width = () => (trackOnly ? sizes.track().width : plotWidth());

  // Rate lanes are binned to the plot's width when this stage's samples change; the buffers are kept.
  const lanes = dirs.map(() => new Lane());
  const binned = signal(0);
  effect(() => {
    props.own();
    const start = props.start();
    const span = props.span();
    const columns = columnsFor(plotWidth());
    for (const [index, lane] of lanes.entries())
      lane.fill(props.samples(), props.stage as TransferStage, dirs[index]!, start, span, columns);
    binned.set(binned.peek() + 1);
  });
  const bars = computed<ReplyBar[]>(() => {
    props.ownBuckets();
    return replyBars(props.buckets(), phase, props.start(), props.span(), width() || 1);
  });
  const hasData = computed(() =>
    trackOnly ? bars().length > 0 : (binned(), lanes.some((lane) => lane.n > 0)),
  );

  // The reading is a time on the timeline, from a pointer or the keyboard.
  const at = signal<number | null>(null);
  let keyboard = false;
  const x = (t: number) =>
    Math.min(width(), Math.max(0, ((t - props.start()) / (props.span() || 1)) * width()));
  /** Gives a reply's height on a track `h` px tall, inside a 1.5 px margin. */
  const trackY = (ms: number, h: number) =>
    h - 1.5 - Math.min(1, Math.max(0, ms / (props.latencyTop() || 1))) * (h - 3);
  const time = (when: number) => fmtDuration(Math.max(0, when - props.start()), 1);
  const reading = computed(() => {
    const t = at();
    if (t === null || !hasData()) return null;
    const reply = (bar: ReplyBar): Row => ({
      label: STAGE.latency.label,
      value: bar.lo < bar.hi ? fmtLatencyRange(bar.lo, bar.hi) : fmtLatency(bar.lo),
      key: "reply",
    });
    if (trackOnly) {
      const found = bars()[nearestIndex(bars().length, (i) => bars()[i]!.t, t)];
      return found
        ? { x: x(found.t), time: time(found.t), rows: [reply(found)], values: [], bar: found }
        : null;
    }
    const nearest = dirs.map((dir) => {
      const samples = props
        .samples()
        .filter(
          (sample) =>
            sample.stage === props.stage && sample.dir === dir && sample.t >= props.start(),
        );
      return samples[nearestIndex(samples.length, (i) => samples[i]!.t, t)] ?? null;
    });
    const first = nearest.find(Boolean);
    if (!first) return null;
    const rows: Row[] = nearest.map((sample, index) => ({
      label: dirs.length > 1 ? STAGE[index ? "upload" : "download"].label : CARDS.rate,
      value: sample ? props.rate(sample.bytesPerSec) : "",
      mark: dirs.length > 1 ? (index ? "↑" : "↓") : undefined,
      key: index ? "second" : "line",
    }));
    const bar = bars().find((each) => first.t >= each.t0 && first.t <= each.t1) ?? null;
    if (bar) rows.push(reply(bar));
    return {
      x: x(first.t),
      time: time(first.t),
      rows,
      values: nearest.map((sample) => sample?.bytesPerSec ?? null),
      bar,
    };
  });

  const graph = (
    <div
      class={trackOnly ? "graph replies" : "graph"}
      data-tone={props.stage}
      role="slider"
      tabindex={() => (hasData() ? 0 : -1)}
      aria-label={props.label}
      aria-valuemin="0"
      aria-valuemax={() => Math.round(props.span())}
      aria-valuenow={() =>
        at() === null ? Math.round(props.span()) : Math.round(at()! - props.start())
      }
      aria-valuetext={() => {
        const now = reading();
        return now
          ? `${now.time}: ${now.rows.map((row) => `${row.label} ${row.value}`).join(", ")}`
          : time(props.start() + props.span());
      }}
      {...scrub(
        (event) => {
          const box = graph.getBoundingClientRect();
          const ratio = Math.min(1, Math.max(0, (event.clientX - box.left) / (box.width || 1)));
          at.set(props.start() + ratio * props.span());
        },
        () => clear(),
      )}
      onpointerleave={(event) => {
        if (event.pointerType === "touch") return;
        if (at.peek() !== null) warmTips();
        clear();
      }}
      onfocus={(event) => {
        if (!event.currentTarget.matches(":focus-visible")) return;
        keyboard = true;
        step("End");
      }}
      onblur={() => {
        keyboard = false;
        at.set(null);
      }}
      onkeydown={(event) => {
        if (event.key === "Escape") {
          if (at.peek() === null) return;
          at.set(null);
        } else if (!step(event.key)) return;
        event.preventDefault();
      }}
    >
      {!trackOnly && plot}
      {track}
    </div>
  ) as HTMLElement;
  function clear(): void {
    if (!keyboard) at.set(null);
  }
  // Arrow keys walk the drawn bins, or the bars of a reply track, from the newest.
  function step(key: string): boolean {
    const lane = lanes.find((each) => each.n > 0);
    const times = trackOnly
      ? bars().map((bar) => bar.t)
      : lane
        ? Array.from(lane.t.subarray(0, lane.n))
        : [];
    if (!times.length) return false;
    const now = at.peek();
    const index = now === null ? times.length : nearestIndex(times.length, (i) => times[i]!, now);
    const delta = STEP[key];
    const next =
      key === "Home"
        ? 0
        : key === "End"
          ? times.length - 1
          : delta === undefined
            ? null
            : index + delta;
    if (next === null) return false;
    at.set(times[Math.min(times.length - 1, Math.max(0, next))]!);
    return true;
  }
  // A new run's strip starts without the last one's reading.
  effect(() => {
    props.start();
    clear();
  });

  // Each reply bar spans a bucket group's fastest to slowest reply, at least a dot's length, on a whole pixel; one
  // over the track's top is clamped there and capped by an arrowhead. Only bars that changed are written.
  const marks: { line: SVGLineElement; over: SVGPathElement | null; key: string }[] = [];
  let medianAt = "";
  effect(() => {
    const { width: w, height: h } = sizes.track();
    trackArt.setAttribute("width", String(w));
    trackArt.setAttribute("height", String(h));
    median.setAttribute("x2", String(w));
    trackCursor.setAttribute("y2", String(h));
  });
  effect(() => {
    const h = sizes.track().height;
    const top = props.latencyTop() || 1;
    const y = (ms: number) => trackY(ms, h);
    const baseline = props.baseline();
    const medianY = baseline === null ? "" : fixed(y(baseline));
    if (medianY !== medianAt) {
      medianAt = medianY;
      shown(median, baseline !== null);
      // Without a median the line hides where it was; an empty length is no coordinate.
      if (baseline !== null) {
        median.setAttribute("y1", medianY);
        median.setAttribute("y2", medianY);
      }
    }
    const list = bars();
    list.forEach((bar, index) => {
      const at = Math.round(x(bar.t));
      const low = y(bar.lo);
      const high = Math.min(y(bar.hi), low - 1.5);
      const clamped = bar.hi > top;
      const key = `${at} ${fixed(low)} ${fixed(high)} ${clamped}`;
      let mark = marks[index];
      if (!mark) {
        mark = { line: svg("line", "reply"), over: null, key: "" };
        trackArt.insertBefore(mark.line, trackCursor);
        marks.push(mark);
      }
      if (mark.key === key) return;
      mark.key = key;
      mark.line.setAttribute("x1", String(at));
      mark.line.setAttribute("x2", String(at));
      setAttr(mark.line, "y1", fixed(low));
      setAttr(mark.line, "y2", fixed(high));
      if (clamped && !mark.over) {
        mark.over = svg("path", "reply-over");
        mark.line.after(mark.over);
      } else if (!clamped && mark.over) {
        mark.over.remove();
        mark.over = null;
      }
      mark.over?.setAttribute("d", `M${at - 2.5} 5.5L${at} 0.5L${at + 2.5} 5.5Z`);
    });
    for (const extra of marks.splice(list.length)) {
      extra.line.remove();
      extra.over?.remove();
    }
  });

  // The readout is a plate beside the cursor on a side it fits, in the half of the plot the curve leaves free.
  const keys: Row["key"][] = [
    ...dirs.map((_, index): Row["key"] => (index ? "second" : "line")),
    "reply",
  ];
  const readout = (
    <span class="inspect-card readout" aria-hidden="true">
      <small>{() => reading()?.time ?? ""}</small>
      {keys.map((key) => {
        const row = computed(() => reading()?.rows.find((each) => each.key === key) ?? null);
        return (
          <span class="row" hidden={() => !row()}>
            <i class="key" data-key={key} />
            {() => {
              const shown = row();
              return shown?.mark ? `${shown.mark} ${shown.value}` : (shown?.value ?? "");
            }}
          </span>
        );
      })}
    </span>
  ) as HTMLElement;
  graph.append(readout);
  // Its width comes from the observer, never read on a move; it hides until known.
  const plate = measure({ readout }).readout;
  let placed = false;
  effect(() => {
    const now = reading();
    readout.classList.toggle("shown", !!now);
    if (!now) {
      placed = false;
      return;
    }
    const wide = plate().width;
    const room = width();
    readout.classList.toggle("unmeasured", !wide);
    const left =
      now.x + GAP + wide <= room
        ? now.x + GAP
        : now.x - GAP - wide >= 0
          ? now.x - GAP - wide
          : Math.max(0, Math.min(room - wide, now.x - wide / 2));
    // A curve in the plot's upper half sends the plate to its foot.
    const value = now.values[0] ?? null;
    readout.classList.toggle("low", value !== null && value / (props.ceiling() || 1) > 0.5);
    // A first reading appears where it reads; the next glide from there.
    if (!placed) readout.style.transition = "none";
    readout.style.translate = `${Math.round(left)}px 0`;
    if (!placed) {
      placed = true;
      requestAnimationFrame(() => (readout.style.transition = ""));
    }
  });

  // The reading marks a dot on each lane's curve and its reply bar drawn over the track.
  const dots = dirs.map((_, index) => svg("circle", index ? "cursor-dot second" : "cursor-dot"));
  for (const dot of dots) {
    dot.setAttribute("r", "3.5");
    shown(dot, false);
  }
  plotArt.append(...dots);
  const replyMark = svg("line", "reply reply-cursor");
  shown(replyMark, false);
  trackArt.append(replyMark);
  effect(() => {
    const now = reading();
    const h = sizes.plot().height;
    const ceiling = props.ceiling() || 1;
    dots.forEach((dot, index) => {
      const value = now?.values[index] ?? null;
      shown(dot, value !== null);
      if (value === null) return;
      setAttr(dot, "cx", fixed(now!.x));
      setAttr(dot, "cy", fixed(h - Math.min(1, Math.max(0, value / ceiling)) * (h - TOP_PAD)));
    });
    const bar = now?.bar ?? null;
    shown(replyMark, !!bar);
    if (!bar) return;
    const th = sizes.track().height;
    const y = (ms: number) => trackY(ms, th);
    const at = String(Math.round(x(bar.t)));
    const low = y(bar.lo);
    setAttr(replyMark, "x1", at);
    setAttr(replyMark, "x2", at);
    setAttr(replyMark, "y1", fixed(low));
    setAttr(replyMark, "y2", fixed(Math.min(y(bar.hi), low - 1.5)));
  });
  effect(() => {
    const now = reading();
    for (const [cursor, height] of [
      [trackCursor, sizes.track().height],
      [plotCursor, sizes.plot().height],
    ] as const) {
      shown(cursor, !!now);
      if (!now) continue;
      const at = String(Math.round(now.x) + 0.5);
      cursor.setAttribute("x1", at);
      cursor.setAttribute("x2", at);
      cursor.setAttribute("y2", String(height));
    }
  });
  if (trackOnly) return graph;

  // The rate plot is a static curve when nothing moves; while live, a frame recomputes the curve's tail only.
  const seen = inView(graph, false);
  let xs = new Float64Array(0);
  let ys = new Float64Array(0);
  let tangents = new Float64Array(0);
  let starts = new Uint32Array(0);
  const head: Head = { t: 0, values: dirs.map(() => 0) };
  const prefixes = dirs.map(() => ({ key: "", path: "", area: "" }));
  let written = 0;
  const closeArea = (i: number, h: number) =>
    `L${fixed(xs[i]!)} ${h}L${fixed(xs[starts[i]!]!)} ${h}Z`;
  const segment = (i: number) => {
    if (starts[i] !== starts[i + 1]) return `M${fixed(xs[i + 1]!)} ${fixed(ys[i + 1]!)}`;
    const third = (xs[i + 1]! - xs[i]!) / 3;
    return `C${fixed(xs[i]! + third)} ${fixed(ys[i]! + tangents[i]! * third)} ${fixed(xs[i + 1]! - third)} ${fixed(
      ys[i + 1]! - tangents[i + 1]! * third,
    )} ${fixed(xs[i + 1]!)} ${fixed(ys[i + 1]!)}`;
  };
  effect(() => {
    const { width: w, height: h } = sizes.plot();
    plotArt.setAttribute("width", String(w));
    plotArt.setAttribute("height", String(h));
    for (const [key, value] of [
      ["x2", String(w)],
      ["y1", String(h - 0.5)],
      ["y2", String(h - 0.5)],
    ] as const)
      axis.setAttribute(key, value);
  });
  function draw(now: number): boolean {
    const w = plotWidth.peek();
    const h = sizes.plot.peek().height;
    const ceiling = props.ceiling() || 1;
    const start = props.start();
    const span = props.span() || 1;
    const px = (t: number) => Math.min(w, Math.max(0, ((t - start) / span) * w));
    const py = (v: number) => h - Math.min(1, Math.max(0, v / ceiling)) * (h - TOP_PAD);
    const leading = props.live() && seen.peek() && props.head(now, head);
    const tip = leading ? px(head.t) : Number.POSITIVE_INFINITY;
    lanes.forEach((lane, index) => {
      if (xs.length < lane.n + 1) {
        xs = new Float64Array(lane.n + 1);
        ys = new Float64Array(lane.n + 1);
        tangents = new Float64Array(lane.n + 1);
        starts = new Uint32Array(lane.n + 1);
      }
      // A live lane ends exactly at its head; bins at or past it wait for it.
      let n = 0;
      while (n < lane.n && px(lane.t[n]!) < tip) {
        xs[n] = px(lane.t[n]!);
        ys[n] = py(lane.v[n]!);
        starts[n] = n === 0 || lane.line[n] !== lane.line[n - 1] ? n : starts[n - 1]!;
        n++;
      }
      const bins = n;
      if (leading && n) {
        xs[n] = tip;
        starts[n] = starts[n - 1]!;
        ys[n++] = py(head.values[index]!);
      }
      const dot = heads[index]!;
      shown(dot, leading && n > 0);
      if (leading && n) {
        setAttr(dot, "cx", fixed(xs[n - 1]!));
        setAttr(dot, "cy", fixed(ys[n - 1]!));
      }
      if (n < 2) {
        setAttr(lines[index]!, "d", "");
        if (index === 0) setAttr(area, "d", "");
        return;
      }
      monotoneTangents(xs, ys, n, tangents, starts);
      // The newest two segments depend on the moving head; the rest stays as it was drawn.
      const tail = Math.min(2, n - 1);
      const prefix = prefixes[index]!;
      const key = `${written}:${bins}:${n - 1 - tail}`;
      if (prefix.key !== key) {
        prefix.key = key;
        prefix.path = `M${fixed(xs[0]!)} ${fixed(ys[0]!)}`;
        prefix.area = prefix.path;
        for (let i = 0; i < n - 1 - tail; i++) {
          const part = segment(i);
          prefix.path += part;
          if (index === 0)
            prefix.area += (starts[i] !== starts[i + 1] ? closeArea(i, h) : "") + part;
        }
      }
      let path = prefix.path;
      let wash = prefix.area;
      for (let i = n - 1 - tail; i < n - 1; i++) {
        const part = segment(i);
        path += part;
        if (index === 0) wash += (starts[i] !== starts[i + 1] ? closeArea(i, h) : "") + part;
      }
      setAttr(lines[index]!, "d", path);
      if (index === 0) setAttr(area, "d", wash + closeArea(n - 1, h));
    });
    return leading && props.active(now);
  }
  const wake = painter(draw);
  effect(() => {
    binned();
    props.ceiling();
    props.live();
    seen();
    written++;
    wake();
  });
  effect(() => {
    sizes.plot();
    written++;
    wake(true);
  });
  return graph;
}
