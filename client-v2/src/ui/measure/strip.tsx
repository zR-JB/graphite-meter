// A card's strip: the stage's rate over time and its reply track underneath, one bar per closed bucket. Each is an SVG
// sized by its attributes and laid out alone. While its stage runs the rate's leading edge follows the frame clock:
// a frame rewrites the newest two curve segments and the head's dot, nothing else. A reading answers a pointer at
// once, a finger on a sideways drag or a tap, and the keyboard bin by bin.
import type { ThroughputSample } from "../../app/series";
import type { Direction } from "../../engine/measure/aggregate";
import type { LatencyBucket } from "../../engine/measure/buckets";
import type { Stage, TransferStage } from "../../engine/run/schedule";
import { computed, effect, onCleanup, signal } from "../../lib/signal";
import { fmtDuration, fmtLatency, fmtLatencyRange } from "../../view/format";
import { STAGE } from "../../view/words";
import { warmTips } from "../kit/tip";
import { CARDS } from "./copy";
import { inView, measure, painter, scrub, setAttr, svg } from "./paint";
import { columnsFor, Lane, monotoneTangents, nearestIndex, replyBars, type ReplyBar } from "./strip-model";

const TOP_PAD = 3;

/** The running stage's leading edge at a frame: now on the timeline and each lane's glided rate (bytes/s). */
export interface Head {
  t: number;
  values: number[];
}

interface StripProps {
  stage: Stage;
  /** Lanes by direction: one for download or upload, two for bidirectional, none for the latency card. */
  dirs: readonly Direction[];
  samples: () => readonly ThroughputSample[];
  /** Changes only when this stage's samples do. */
  own: () => unknown;
  buckets: () => readonly LatencyBucket[];
  /** Changes only when this stage's buckets do. */
  ownBuckets: () => unknown;
  start: () => number;
  span: () => number;
  /** Bytes/s at the plot's top and ms at the track's, shared by every strip so heights compare. */
  ceiling: () => number;
  latencyTop: () => number;
  /** The idle median, drawn across the track. */
  baseline: () => number | null;
  rate: (bytesPerSec: number) => string;
  /** The stage runs on its own rates: its leading edge moves every frame. */
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
}

let strips = 0;

const shown = (mark: SVGElement, visible: boolean) => setAttr(mark, "visibility", visible ? "visible" : "hidden");
const fixed = (value: number) => (Math.round(value * 100) / 100).toString();

export function StageGraph(props: StripProps): HTMLElement {
  const { dirs } = props;
  const trackOnly = dirs.length === 0;
  const phase = props.stage as LatencyBucket["phase"];

  // The track: the idle median's dashed line, one bar per joined bucket, the cursor.
  const track = (<div class={trackOnly ? "track full" : "track"} />) as HTMLElement;
  const trackArt = svg("svg", "art");
  trackArt.setAttribute("aria-hidden", "true");
  const median = svg("line", "reply-median");
  const trackCursor = svg("line", "cursor");
  trackArt.append(median, trackCursor);
  track.append(trackArt);
  shown(median, false);
  shown(trackCursor, false);

  // The plot: lane 0 over a wash of its hue that fades down from the line, the second lane dashed, the heads' dots.
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

  // Rate lanes binned to the plot's width when this stage's samples change; the buffers are kept.
  const lanes = dirs.map(() => new Lane());
  const binned = signal(0);
  effect(() => {
    props.own();
    const start = props.start();
    const span = props.span();
    const columns = columnsFor(plotWidth());
    lanes.forEach((lane, index) =>
      lane.fill(props.samples(), props.stage as TransferStage, dirs[index]!, start, span, columns),
    );
    binned.set(binned.peek() + 1);
  });
  const bars = computed<ReplyBar[]>(() => {
    props.ownBuckets();
    return replyBars(props.buckets(), phase, props.start(), props.span(), width() || 1);
  });
  const hasData = computed(() => (trackOnly ? bars().length > 0 : (binned(), lanes.some((lane) => lane.n > 0))));

  // The reading: a time on the timeline, from a pointer or the keyboard.
  const at = signal<number | null>(null);
  let keyboard = false;
  const x = (t: number) => Math.min(width(), Math.max(0, ((t - props.start()) / (props.span() || 1)) * width()));
  const reading = computed(() => {
    const t = at();
    if (t === null || !hasData()) return null;
    const time = (when: number) => fmtDuration(Math.max(0, when - props.start()), 1);
    const reply = (bar: ReplyBar): Row => ({
      label: STAGE.latency.label,
      value: bar.lo < bar.hi ? fmtLatencyRange(bar.lo, bar.hi) : fmtLatency(bar.lo),
    });
    if (trackOnly) {
      const found = bars()[nearestIndex(bars().length, (i) => bars()[i]!.t, t)];
      return found ? { x: x(found.t), time: time(found.t), rows: [reply(found)] } : null;
    }
    const nearest = dirs.map((dir) => {
      const samples = props
        .samples()
        .filter((sample) => sample.stage === props.stage && sample.dir === dir && sample.t >= props.start());
      return samples[nearestIndex(samples.length, (i) => samples[i]!.t, t)] ?? null;
    });
    const first = nearest.find(Boolean);
    if (!first) return null;
    const rows: Row[] = nearest.map((sample, index) => ({
      label: dirs.length > 1 ? STAGE[index ? "upload" : "download"].label : CARDS.rate,
      value: sample ? props.rate(sample.bytesPerSec) : "",
      mark: dirs.length > 1 ? (index ? "↑" : "↓") : undefined,
    }));
    const bar = bars().find((each) => first.t >= each.t0 && first.t <= each.t1);
    if (bar) rows.push(reply(bar));
    return { x: x(first.t), time: time(first.t), rows };
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
      aria-valuenow={() => (at() === null ? Math.round(props.span()) : Math.round(at()! - props.start()))}
      aria-valuetext={() => {
        const now = reading();
        return now ? `${now.time}: ${now.rows.map((row) => `${row.label} ${row.value}`).join(", ")}` : undefined;
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
    const times = trackOnly ? bars().map((bar) => bar.t) : lane ? Array.from(lane.t.subarray(0, lane.n)) : [];
    if (!times.length) return false;
    const now = at.peek();
    const index = now === null ? times.length : nearestIndex(times.length, (i) => times[i]!, now);
    const next = { ArrowLeft: index - 1, ArrowRight: index + 1, Home: 0, End: times.length - 1 }[key];
    if (next === undefined) return false;
    at.set(times[Math.min(times.length - 1, Math.max(0, next))]!);
    return true;
  }
  // A new run's strip starts without the last one's reading.
  effect(() => {
    props.start();
    clear();
  });

  // Reply bars: each a bucket group's fastest to slowest reply, at least a dot's length, on a whole pixel; one over
  // the track's top is clamped there and capped by an arrowhead. Only bars that changed are written.
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
    const y = (ms: number) => h - 1.5 - Math.min(1, Math.max(0, ms / top)) * (h - 3);
    const baseline = props.baseline();
    const medianY = baseline === null ? "" : fixed(y(baseline));
    if (medianY !== medianAt) {
      medianAt = medianY;
      shown(median, baseline !== null);
      median.setAttribute("y1", medianY);
      median.setAttribute("y2", medianY);
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

  // The readout sits in the top layer over the card's quiet line, centred on the cursor inside the strip, and keeps
  // to the strip as the page scrolls.
  const open = computed(() => reading() !== null);
  effect(() => {
    if (!open()) return;
    const readout = (
      <span class="inspect-card readout unmeasured" popover="manual">
        {() => {
          const now = reading();
          return (
            now && [
              <small>{now.time}</small>,
              now.rows.map((row) => <span>{row.mark ? [<small>{row.mark}</small>, ` ${row.value}`] : row.value}</span>),
            ]
          );
        }}
      </span>
    ) as HTMLElement;
    graph.append(readout);
    readout.showPopover();
    const place = () => {
      const now = reading.peek();
      if (!now) return;
      const box = graph.getBoundingClientRect();
      const measured = readout.offsetWidth;
      readout.style.maxWidth = `${box.width}px`;
      readout.style.left = `${Math.round(box.left + Math.max(0, Math.min(now.x - measured / 2, box.width - measured)))}px`;
      readout.style.top = `${box.top}px`;
      readout.classList.remove("unmeasured");
    };
    effect(() => {
      reading();
      place();
    });
    addEventListener("scroll", place, { capture: true, passive: true });
    addEventListener("resize", place);
    onCleanup(() => {
      removeEventListener("scroll", place, { capture: true });
      removeEventListener("resize", place);
      readout.remove();
    });
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

  // The rate plot: a static curve when nothing moves; while live, a frame recomputes the curve's tail only.
  const seen = inView(graph, false);
  let xs = new Float64Array(0);
  let ys = new Float64Array(0);
  let tangents = new Float64Array(0);
  const head: Head = { t: 0, values: dirs.map(() => 0) };
  const prefixes = dirs.map(() => ({ key: "", path: "" }));
  let written = 0;
  const segment = (i: number) => {
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
      }
      // A live lane ends exactly at its head; bins at or past it wait for it.
      let n = 0;
      while (n < lane.n && px(lane.t[n]!) < tip) {
        xs[n] = px(lane.t[n]!);
        ys[n] = py(lane.v[n]!);
        n++;
      }
      const bins = n;
      if (leading && n) {
        xs[n] = tip;
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
      monotoneTangents(xs, ys, n, tangents);
      // The newest two segments depend on the moving head; the rest stays as it was drawn.
      const tail = Math.min(2, n - 1);
      const prefix = prefixes[index]!;
      const key = `${written}:${bins}:${n - 1 - tail}`;
      if (prefix.key !== key) {
        prefix.key = key;
        prefix.path = `M${fixed(xs[0]!)} ${fixed(ys[0]!)}`;
        for (let i = 0; i < n - 1 - tail; i++) prefix.path += segment(i);
      }
      let path = prefix.path;
      for (let i = n - 1 - tail; i < n - 1; i++) path += segment(i);
      setAttr(lines[index]!, "d", path);
      if (index === 0) setAttr(area, "d", `${path}L${fixed(xs[n - 1]!)} ${h}L${fixed(xs[0]!)} ${h}Z`);
    });
    return leading && props.active(now);
  }
  const wake = painter(draw);
  effect(() => {
    binned();
    props.ceiling();
    sizes.plot();
    props.live();
    seen();
    written++;
    wake();
  });
  return graph;
}
