// Dual-axis timeseries. It redraws only for data, camera, theme, size, or hover
// invalidations; the shared presentation scheduler owns the frame clock.

import type {
  Phase,
  ThroughputSample,
  LatencyBucket,
} from "../runner/contract";
import type { CanvasEngine } from "./contract";
import { sharedThroughputScale } from "../format";
import {
  latencyBucketExceedsScale,
  latencyScaleForHistory,
} from "../runner/latencyScale";
import {
  hasHoverMeasurements,
  interpolateConnectedAt,
  lowerBoundAt,
} from "./hoverInterp";
import { throughputSamplesContinuous } from "./throughputContinuity";
import { presentation, type PresentationHandle } from "./presentation";
import { LatencyPhaseIndex } from "./latencyPhaseIndex";
import { latencyOverflowGlyph, nearestLatencyGlyph } from "./latencyGlyph";
import {
  chartLayout,
  type ChartLayout,
  type ChartViewport,
} from "./chartLayout";

export interface ChartData {
  throughput: ThroughputSample[];
  latency: LatencyBucket[];
  /** Increments when latency history changes anywhere except a pure tail
   *  append, invalidating incremental chart indexes. */
  latencyRevision: number;
  /** False when latency is disabled: the latency line and right axis are
   *  suppressed. */
  latencyEnabled: boolean;
  phase: Phase;
  /** Exact phase boundary on the runner's measured timeline. */
  phaseStartedAtMs: number;
  /** Monotonic run counter. A change resets all per-run engine state. */
  runSeq: number;
  /** Throughput Y-axis ceiling (bytes/s), dwell-filtered and tiered upstream.
   *  Identical to store.displayScaleBytesPerSec so the gauge dial matches. */
  scaleBytesPerSec: number;
  /** Shared robust latency ceiling, identical to the gauge. */
  latencyScaleMs: number;
  /** Canonical headline rates produced by the measurement reducer. */
  resultRates: Partial<
    Record<"download" | "upload" | "bidiDown" | "bidiUp", number>
  >;
}

export interface HoverInfo {
  x: number; // clamped css px within plot
  t: number; // ms
  /** Single-lane rate during download/upload. Null during bidirectional and
   *  outside the throughput data. */
  bytesPerSec: number | null;
  /** Bidirectional's two concurrent lanes; null outside that phase. */
  downBytesPerSec: number | null;
  upBytesPerSec: number | null;
  /** The real bucket glyph selected near the pointer, never an interpolated RTT. */
  latencyX: number | null;
  rtt: number | null;
  pingCount: number;
  lossCount: number;
  latencyOverflow: boolean;
}

/** Per-lane average overlay drawn in result mode. */
interface PhaseStat {
  t0: number;
  t1: number;
  avg: number;
  stroke: string;
}

interface PhaseSpan {
  phase: Phase;
  t0: number;
  t1: number; // Infinity while open
}

type ThroughputLane = "download" | "upload" | "bidiDown" | "bidiUp";

export interface ChartPresentation {
  layout: ChartLayout;
  latencyEnabled: boolean;
  hasThroughputScale: boolean;
  phaseLabels: ReadonlyArray<{ phase: ChartLabelPhase; x: number; y: number }>;
  phaseStats: ReadonlyArray<{
    bytesPerSec: number;
    stroke: string;
    x: number;
    y: number;
  }>;
}

export type ChartLabelPhase = Extract<
  Phase,
  "warmup" | "latency" | "download" | "upload" | "bidirectional"
>;

function isChartLabelPhase(phase: Phase): phase is ChartLabelPhase {
  return (
    phase === "warmup" ||
    phase === "latency" ||
    phase === "download" ||
    phase === "upload" ||
    phase === "bidirectional"
  );
}

interface ThemeColors {
  download: string;
  downloadRgb: { r: number; g: number; b: number };
  upload: string;
  uploadRgb: { r: number; g: number; b: number };
  bidirectional: string;
  warmup: string;
  signal: string;
  warn: string;
  grid: string;
  textSoft: string;
  brand: string;
}

function hexToRgb(hex: string): { r: number; g: number; b: number } {
  const h = hex.replace("#", "").trim();
  const full =
    h.length === 3
      ? h
          .split("")
          .map((c) => c + c)
          .join("")
      : h;
  const n = parseInt(full || "888888", 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

export class ChartEngine implements CanvasEngine {
  #get: () => ChartData;
  #onPresentation: ((presentation: ChartPresentation) => void) | null;
  #canvas: HTMLCanvasElement | null = null;
  #ctx: CanvasRenderingContext2D | null = null;
  #scene: HTMLCanvasElement | null = null;
  #sceneCtx: CanvasRenderingContext2D | null = null;
  #sceneDirty = true;
  #presentation: PresentationHandle | null = null;

  #dpr = 1;
  #w = 0;
  #h = 0;

  #vp: ChartViewport = {
    tMin: 0,
    tMax: 6000,
    bytesPerSecMax: 125_000,
    rttMin: 0,
    rttMax: 50,
  };
  #vpInit = false;
  #layout = chartLayout(1, 1, this.#vp);

  // Rebuilt only when theme or plot height changes.
  #gradDownload: CanvasGradient | null = null;
  #gradUpload: CanvasGradient | null = null;
  #gradH = -1;
  // Phase boundaries only; each sample carries its own phase for attribution.
  #spans: PhaseSpan[] = [];
  #lastPhase: Phase | null = null;
  #hoverX: number | null = null;
  #result = false; // frozen post-run result mode
  #runSeq = -1; // last-seen store.runSeq; a change triggers a full reset
  #hasThroughputScale = false;
  #indexedThroughput = 0;
  #lastIndexedThroughput: ThroughputSample | undefined;
  #throughputByLane: Record<ThroughputLane, ThroughputSample[]> = {
    download: [],
    upload: [],
    bidiDown: [],
    bidiUp: [],
  };
  #latencyIndex = new LatencyPhaseIndex();

  #colors: ThemeColors = {
    download: "#6db0b8",
    downloadRgb: { r: 109, g: 176, b: 184 },
    upload: "#bda36c",
    uploadRgb: { r: 189, g: 163, b: 108 },
    bidirectional: "#a695c8",
    warmup: "#858c94",
    signal: "#8ba3ba",
    warn: "#c4a568",
    grid: "rgba(211,219,227,0.05)",
    textSoft: "#6a717a",
    brand: "#6db0b8",
  };

  constructor(
    get: () => ChartData,
    onPresentation?: (presentation: ChartPresentation) => void,
  ) {
    this.#get = get;
    this.#onPresentation = onPresentation ?? null;
  }

  attach(canvas: HTMLCanvasElement): void {
    this.#canvas = canvas;
    this.#ctx = canvas.getContext("2d");
    this.#scene = document.createElement("canvas");
    this.#sceneCtx = this.#scene.getContext("2d");
    this.#presentation = presentation.register(canvas, this.#render);
    this.invalidateTheme();
  }

  wake(): void {
    this.#sceneDirty = true;
    this.#presentation?.invalidate();
  }

  destroy(): void {
    this.#presentation?.destroy();
    this.#presentation = null;
    this.#canvas = null;
    this.#ctx = null;
    this.#scene = null;
    this.#sceneCtx = null;
    this.#spans = [];
  }

  invalidateTheme(): void {
    if (!this.#canvas || !this.#ctx) return;
    // Cap at 2: beyond that the raster cost grows without visible fidelity.
    this.#dpr = Math.min(window.devicePixelRatio || 1, 2);
    const rect = this.#canvas.getBoundingClientRect();
    this.#w = Math.max(1, rect.width);
    this.#h = Math.max(1, rect.height);
    this.#canvas.width = Math.round(this.#w * this.#dpr);
    this.#canvas.height = Math.round(this.#h * this.#dpr);
    this.#ctx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    if (this.#scene && this.#sceneCtx) {
      this.#scene.width = this.#canvas.width;
      this.#scene.height = this.#canvas.height;
      this.#sceneCtx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    }
    this.#resolveColors();
    this.wake();
  }

  setHover(x: number | null): void {
    this.#hoverX = x;
    this.#presentation?.invalidate();
  }

  hoverInfo(): HoverInfo | null {
    if (this.#hoverX == null) return null;
    const plotW = this.#layout.plot.right - this.#layout.plot.left;
    if (plotW <= 0) return null;
    const x = Math.max(
      this.#layout.plot.left,
      Math.min(this.#layout.plot.right, this.#hoverX),
    );
    const frac = (x - this.#layout.plot.left) / plotW;
    const t = this.#vp.tMin + frac * (this.#vp.tMax - this.#vp.tMin);
    const data = this.#get();
    this.#indexData(data);
    const bytesPerSec =
      interpolateConnectedAt(
        this.#throughputByLane.download,
        t,
        (s) => s.bytesPerSec,
        throughputSamplesContinuous,
      ) ??
      interpolateConnectedAt(
        this.#throughputByLane.upload,
        t,
        (s) => s.bytesPerSec,
        throughputSamplesContinuous,
      );
    const downBytesPerSec = interpolateConnectedAt(
      this.#throughputByLane.bidiDown,
      t,
      (s) => s.bytesPerSec,
      throughputSamplesContinuous,
    );
    const upBytesPerSec = interpolateConnectedAt(
      this.#throughputByLane.bidiUp,
      t,
      (s) => s.bytesPerSec,
      throughputSamplesContinuous,
    );
    const latencyGlyph = data.latencyEnabled
      ? nearestLatencyGlyph(this.#latencyIndex.values(), x, (bucketT) =>
          this.#x(bucketT),
        )
      : null;
    const latencyBucket = latencyGlyph?.bucket ?? null;
    const latencyX = latencyGlyph?.x ?? null;
    const rtt = latencyBucket?.medianRttMs ?? null;
    const info: HoverInfo = {
      x,
      t,
      bytesPerSec,
      downBytesPerSec,
      upBytesPerSec,
      latencyX,
      rtt,
      pingCount: latencyBucket?.pingCount ?? 0,
      lossCount: latencyBucket?.lossCount ?? 0,
      latencyOverflow:
        latencyBucket != null &&
        latencyBucketExceedsScale(latencyBucket, this.#vp.rttMax),
    };
    return hasHoverMeasurements(info) ? info : null;
  }

  #resolveColors(): void {
    const cs = getComputedStyle(document.documentElement);
    const g = (v: string, fb: string) => cs.getPropertyValue(v).trim() || fb;
    const download = g("--phase-download", "#6db0b8");
    const upload = g("--phase-upload", "#bda36c");
    this.#colors = {
      download,
      downloadRgb: hexToRgb(download),
      upload,
      uploadRgb: hexToRgb(upload),
      bidirectional: g("--phase-bidirectional", "#a695c8"),
      warmup: g("--phase-warmup", "#858c94"),
      signal: g("--signal", "#8ba3ba"),
      warn: g("--warn", "#c4a568"),
      grid: g("--grid-line", "rgba(211,219,227,0.05)"),
      textSoft: g("--text-soft", "#6a717a"),
      brand: g("--brand", "#6db0b8"),
    };
    this.#invalidateGradients();
  }

  #invalidateGradients(): void {
    this.#gradH = -1;
  }

  #areaGrad(
    ctx: CanvasRenderingContext2D,
    phase: "download" | "upload",
  ): CanvasGradient {
    if (this.#gradH !== this.#h) {
      const bot = this.#layout.plot.bottom;
      const make = (rgb: {
        r: number;
        g: number;
        b: number;
      }): CanvasGradient => {
        const grad = ctx.createLinearGradient(0, this.#layout.plot.top, 0, bot);
        grad.addColorStop(0, `rgba(${rgb.r},${rgb.g},${rgb.b},0.22)`);
        grad.addColorStop(1, `rgba(${rgb.r},${rgb.g},${rgb.b},0)`);
        return grad;
      };
      this.#gradDownload = make(this.#colors.downloadRgb);
      this.#gradUpload = make(this.#colors.uploadRgb);
      this.#gradH = this.#h;
    }
    return phase === "download" ? this.#gradDownload! : this.#gradUpload!;
  }

  #render = (): boolean => {
    if (this.#sceneDirty) {
      this.#update();
      this.#drawScene();
      this.#sceneDirty = false;
    }
    this.#compose();
    return false;
  };

  #latestT(d: ChartData): number {
    const a = d.throughput.length ? d.throughput[d.throughput.length - 1].t : 0;
    const b = d.latency.length ? d.latency[d.latency.length - 1].t : 0;
    return Math.max(a, b);
  }

  #resetRunState(): void {
    this.#spans = [];
    this.#lastPhase = null;
    this.#vpInit = false;
    this.#result = false;
    this.#hasThroughputScale = false;
    this.#indexedThroughput = 0;
    this.#lastIndexedThroughput = undefined;
    for (const lane of Object.values(this.#throughputByLane)) lane.length = 0;
    this.#latencyIndex.clear();
  }

  #update(): void {
    const d = this.#get();

    if (d.runSeq !== this.#runSeq) {
      this.#runSeq = d.runSeq;
      this.#resetRunState();
    }
    this.#indexData(d);

    // Sample timestamps cannot mark a sample-free warmup, so the runner's
    // boundary is the phase clock.
    if (d.phase !== this.#lastPhase) {
      const phaseStart =
        d.phase === "complete" || d.phase === "error"
          ? this.#latestT(d)
          : d.phaseStartedAtMs;
      if (this.#spans.length)
        this.#spans[this.#spans.length - 1].t1 = phaseStart;
      this.#spans.push({ phase: d.phase, t0: phaseStart, t1: Infinity });
      this.#lastPhase = d.phase;

      if (this.#vpInit) this.#vp.tMin = Math.max(this.#vp.tMin, phaseStart);
    }

    const latest = this.#latestT(d);
    const complete =
      d.phase === "complete" || d.phase === "aborted" || d.phase === "error";
    this.#result = complete;

    let tMin: number;
    let tMax: number;
    if (complete) {
      tMin = 0;
      tMax = Math.max(latest * 1.02, 1000);
    } else {
      const span = this.#spans[this.#spans.length - 1];
      const phaseStart = span ? span.t0 : 0;
      tMin = phaseStart;
      tMax = Math.max(latest + 2000, phaseStart + 4000);
    }

    const bytesPerSecMax =
      d.scaleBytesPerSec > 0 ? d.scaleBytesPerSec : 125_000;
    this.#hasThroughputScale =
      d.scaleBytesPerSec !== sharedThroughputScale(0) ||
      d.throughput.length > 0;

    const rttMin = 0;
    // The shared controller is intentionally recent while measuring. Terminal
    // mode expands the X axis to the full run, so resolve its Y domain from the
    // same full history instead of applying the last live window retroactively.
    const rttMax = complete
      ? latencyScaleForHistory(d.latency)
      : d.latencyScaleMs;

    this.#vp = { tMin, tMax, bytesPerSecMax, rttMin, rttMax };
    this.#layout = chartLayout(this.#w, this.#h, this.#vp);
    this.#vpInit = true;
    this.#publishPresentation(d);
  }

  /* Coordinate maps. */
  #x(t: number): number {
    return this.#layout.x(t);
  }
  #yL(bytesPerSec: number): number {
    return this.#layout.throughputY(bytesPerSec);
  }
  #yR(rtt: number): number {
    return this.#layout.latencyY(rtt);
  }

  #publishPresentation(data: ChartData): void {
    if (!this.#onPresentation) return;
    const { plot } = this.#layout;
    let warmupLabelled = false;
    const phaseLabels = this.#result
      ? this.#spans.flatMap((span) => {
          const x0 = Math.max(plot.left, this.#x(span.t0));
          const x1 = Math.min(
            plot.right,
            this.#x(span.t1 === Infinity ? this.#vp.tMax : span.t1),
          );
          const width = x1 - x0 - 2;
          const repeatWarmup = span.phase === "warmup" && warmupLabelled;
          if (span.phase === "warmup") warmupLabelled = true;
          return width > 56 && !repeatWarmup && isChartLabelPhase(span.phase)
            ? [{ phase: span.phase, x: x0 + 3, y: plot.top + 9 }]
            : [];
        })
      : [];
    const phaseStats = this.#result
      ? this.#phaseStats(data.throughput).flatMap((stat) => {
          const x0 = Math.max(plot.left, this.#x(stat.t0));
          const x1 = Math.min(plot.right, this.#x(stat.t1));
          if (x1 <= x0) return [];
          const y = this.#yL(stat.avg);
          return [
            {
              bytesPerSec: stat.avg,
              stroke: stat.stroke,
              x: Math.min(x0 + 3, plot.right - 130),
              y: y - 4 - 14 < plot.top ? y + 4 : y - 4 - 14,
            },
          ];
        })
      : [];
    this.#onPresentation({
      layout: this.#layout,
      latencyEnabled: data.latencyEnabled,
      hasThroughputScale: this.#hasThroughputScale,
      phaseLabels,
      phaseStats,
    });
  }

  #drawScene(): void {
    const ctx = this.#sceneCtx;
    if (!ctx) return;
    const d = this.#get();
    ctx.clearRect(0, 0, this.#w, this.#h);

    this.#drawGrid(ctx);
    this.#drawThroughput(ctx, d.throughput);
    if (this.#result) this.#drawPhaseStats(ctx, d.throughput);
    if (d.latencyEnabled) this.#drawLatency(ctx, d.latency);
    this.#drawPhases(ctx);
  }

  #compose(): void {
    const ctx = this.#ctx;
    if (!ctx || !this.#scene) return;
    ctx.clearRect(0, 0, this.#w, this.#h);
    ctx.drawImage(
      this.#scene,
      0,
      0,
      this.#scene.width,
      this.#scene.height,
      0,
      0,
      this.#w,
      this.#h,
    );
    this.#drawHover(ctx);
  }

  /** Ribbon colour, null when the phase shows none. Warmup uses the
   *  muted body-text grey so it recedes behind the lane colours. */
  #phaseColor(phase: Phase): string | null {
    if (phase === "warmup") return this.#colors.textSoft;
    if (phase === "latency") return this.#colors.signal;
    if (phase === "download") return this.#colors.download;
    if (phase === "upload") return this.#colors.upload;
    if (phase === "bidirectional") return this.#colors.bidirectional;
    return null;
  }

  #drawPhaseStats(
    ctx: CanvasRenderingContext2D,
    all: ThroughputSample[],
  ): void {
    for (const stat of this.#phaseStats(all)) {
      const x0 = Math.max(this.#layout.plot.left, this.#x(stat.t0));
      const x1 = Math.min(this.#layout.plot.right, this.#x(stat.t1));
      if (x1 <= x0) continue;
      const yAvg = this.#yL(stat.avg);

      ctx.save();
      ctx.strokeStyle = stat.stroke;
      ctx.globalAlpha = 0.8;
      ctx.lineWidth = 1.25;
      ctx.setLineDash([4, 4]);
      ctx.beginPath();
      ctx.moveTo(x0, Math.round(yAvg) + 0.5);
      ctx.lineTo(x1, Math.round(yAvg) + 0.5);
      ctx.stroke();
      ctx.restore();
    }
  }

  #phaseStats(all: ThroughputSample[]): PhaseStat[] {
    const out: PhaseStat[] = [];
    for (const phase of ["download", "upload"] as const) {
      const seg = all.filter((s) => s.phase === phase);
      const average = this.#get().resultRates[phase];
      if (seg.length < 2 || average == null) continue;
      out.push({
        t0: seg[0].t,
        t1: seg[seg.length - 1].t,
        avg: average,
        stroke:
          phase === "download" ? this.#colors.download : this.#colors.upload,
      });
    }
    for (const dir of ["down", "up"] as const) {
      const seg = all.filter(
        (s) => s.phase === "bidirectional" && s.dir === dir,
      );
      const average =
        this.#get().resultRates[dir === "down" ? "bidiDown" : "bidiUp"];
      if (seg.length < 2 || average == null) continue;
      out.push({
        t0: seg[0].t,
        t1: seg[seg.length - 1].t,
        avg: average,
        stroke: dir === "down" ? this.#colors.download : this.#colors.upload,
      });
    }
    return out;
  }

  #drawPhases(ctx: CanvasRenderingContext2D): void {
    const ry = this.#layout.phaseRailY;
    for (const s of this.#spans) {
      const color = this.#phaseColor(s.phase);
      if (!color) continue;
      const x0 = Math.max(this.#layout.plot.left, this.#x(s.t0));
      const x1 = Math.min(
        this.#layout.plot.right,
        this.#x(s.t1 === Infinity ? this.#vp.tMax : s.t1),
      );
      const w = x1 - x0 - 2;
      if (w < 3) continue;

      ctx.globalAlpha = 0.85;
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.roundRect(x0, ry, w, 3, 1.5);
      ctx.fill();
    }
    ctx.globalAlpha = 1;
  }

  #drawGrid(ctx: CanvasRenderingContext2D): void {
    const { plot } = this.#layout;

    ctx.lineWidth = 1;
    ctx.strokeStyle = this.#colors.grid;
    ctx.globalAlpha = 0.55;
    ctx.beginPath();
    for (const tick of this.#layout.timeMinorTicks) {
      ctx.moveTo(tick.x, plot.top);
      ctx.lineTo(tick.x, plot.bottom);
    }
    for (const y of this.#layout.horizontalMinorLines) {
      ctx.moveTo(plot.left, y);
      ctx.lineTo(plot.right, y);
    }
    ctx.stroke();
    ctx.globalAlpha = 1;

    ctx.strokeStyle = this.#colors.grid;
    ctx.beginPath();
    for (const tick of this.#layout.timeMajorTicks) {
      ctx.moveTo(tick.x, plot.top);
      ctx.lineTo(tick.x, plot.bottom);
    }
    for (const y of this.#layout.horizontalMajorLines) {
      ctx.moveTo(plot.left, y);
      ctx.lineTo(plot.right, y);
    }
    ctx.stroke();
  }

  #indexData(data: ChartData): void {
    const all = data.throughput;
    if (
      all.length < this.#indexedThroughput ||
      (all.length === this.#indexedThroughput &&
        all.at(-1) !== this.#lastIndexedThroughput)
    ) {
      this.#indexedThroughput = 0;
      for (const lane of Object.values(this.#throughputByLane)) lane.length = 0;
    }
    for (let i = this.#indexedThroughput; i < all.length; i++) {
      const sample = all[i];
      const lane: ThroughputLane =
        sample.phase === "bidirectional"
          ? sample.dir === "down"
            ? "bidiDown"
            : "bidiUp"
          : sample.phase;
      this.#throughputByLane[lane].push(sample);
    }
    this.#indexedThroughput = all.length;
    this.#lastIndexedThroughput = all.at(-1);

    this.#latencyIndex.update(data.latency, data.latencyRevision);
  }

  #throughputLanes(): {
    samples: ThroughputSample[];
    area: "download" | "upload";
  }[] {
    return [
      { samples: this.#throughputByLane.download, area: "download" },
      { samples: this.#throughputByLane.upload, area: "upload" },
      { samples: this.#throughputByLane.bidiDown, area: "download" },
      { samples: this.#throughputByLane.bidiUp, area: "upload" },
    ];
  }

  #drawThroughput(
    ctx: CanvasRenderingContext2D,
    all: ThroughputSample[],
  ): void {
    if (!all.length) return;
    const bot = this.#layout.plot.bottom;
    const tMin = this.#vp.tMin;
    const tMax = this.#vp.tMax;
    for (const lane of this.#throughputLanes()) {
      const stroke =
        lane.area === "download" ? this.#colors.download : this.#colors.upload;
      const segments: { x: number; y: number }[][] = [];
      let pts: { x: number; y: number }[] = [];
      let previous: ThroughputSample | null = null;
      const lo = Math.max(0, lowerBoundAt(lane.samples, tMin) - 1);
      const hi = Math.min(
        lane.samples.length,
        lowerBoundAt(lane.samples, tMax) + 1,
      );
      for (let i = lo; i < hi; i++) {
        const s = lane.samples[i];
        if (previous && !throughputSamplesContinuous(previous, s)) {
          if (pts.length) segments.push(pts);
          pts = [];
        }
        pts.push({ x: this.#x(s.t), y: this.#yL(s.bytesPerSec) });
        previous = s;
      }
      if (pts.length) segments.push(pts);
      for (const segment of segments) {
        if (segment.length < 2) continue;
        ctx.fillStyle = this.#areaGrad(ctx, lane.area);
        ctx.beginPath();
        ctx.moveTo(segment[0].x, bot);
        ctx.lineTo(segment[0].x, segment[0].y);
        for (let i = 1; i < segment.length; i++)
          ctx.lineTo(segment[i].x, segment[i].y);
        ctx.lineTo(segment.at(-1)!.x, bot);
        ctx.closePath();
        ctx.fill();

        ctx.strokeStyle = stroke;
        ctx.lineWidth = 1.75;
        ctx.lineJoin = "round";
        ctx.lineCap = "round";
        ctx.beginPath();
        ctx.moveTo(segment[0].x, segment[0].y);
        for (let i = 1; i < segment.length; i++)
          ctx.lineTo(segment[i].x, segment[i].y);
        ctx.stroke();
      }
    }
  }

  #drawLatency(ctx: CanvasRenderingContext2D, all: LatencyBucket[]): void {
    if (!all.length) return;
    ctx.lineWidth = 1;
    const lo = Math.max(0, lowerBoundAt(all, this.#vp.tMin) - 1);
    const hi = Math.min(all.length, lowerBoundAt(all, this.#vp.tMax) + 1);
    for (let i = lo; i < hi; i++) {
      const s = all[i];
      const color = s.underLoad ? this.#colors.warn : this.#colors.signal;
      if (s.medianRttMs != null) {
        const x = this.#x(s.t);
        const overflow = latencyBucketExceedsScale(s, this.#vp.rttMax);
        const clippedMedian = s.medianRttMs >= this.#vp.rttMax;
        const overflowGlyph = overflow
          ? latencyOverflowGlyph(this.#layout.plot.top)
          : null;
        const medianY =
          clippedMedian && overflowGlyph
            ? overflowGlyph.dot.y
            : this.#yR(s.medianRttMs);
        const spike = s.maxRttMs ?? s.p95RttMs;
        if (spike != null && spike > s.medianRttMs && !clippedMedian) {
          ctx.strokeStyle = color;
          ctx.beginPath();
          ctx.moveTo(x, medianY);
          ctx.lineTo(x, this.#yR(spike));
          ctx.stroke();
        }
        ctx.fillStyle = color;
        ctx.beginPath();
        ctx.arc(x, medianY, 2.25, 0, Math.PI * 2);
        ctx.fill();
        if (overflowGlyph) {
          ctx.fillStyle = color;
          ctx.beginPath();
          ctx.moveTo(x, overflowGlyph.arrow.tipY);
          ctx.lineTo(
            x - overflowGlyph.arrow.halfWidth,
            overflowGlyph.arrow.baseY,
          );
          ctx.lineTo(
            x + overflowGlyph.arrow.halfWidth,
            overflowGlyph.arrow.baseY,
          );
          ctx.closePath();
          ctx.fill();
        }
      }
      if (s.lossCount > 0) {
        const x = this.#x(s.t);
        ctx.fillStyle = this.#colors.warn;
        ctx.fillRect(x - 1.5, this.#layout.plot.bottom - 5, 3, 5);
      }
    }
  }

  #drawHover(ctx: CanvasRenderingContext2D): void {
    if (this.#hoverX == null) return;
    const x = Math.max(
      this.#layout.plot.left,
      Math.min(this.#layout.plot.right, this.#hoverX),
    );
    const { top, bottom: bot } = this.#layout.plot;
    ctx.strokeStyle = this.#colors.brand;
    ctx.lineWidth = 1;
    const gx = Math.round(x) + 0.5;
    ctx.beginPath();
    ctx.moveTo(gx, top);
    ctx.lineTo(gx, bot);
    ctx.stroke();

    const info = this.hoverInfo();
    if (!info) return;
    const dot = (y: number, color: string): void => {
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(x, y, 2.5, 0, Math.PI * 2);
      ctx.fill();
    };
    // Dots ride the interpolated value (matches the chip + the drawn line).
    if (info.bytesPerSec != null)
      dot(this.#yL(info.bytesPerSec), this.#colors.brand);
    // Bidirectional: one dot per lane, tinted to match its drawn line.
    if (info.downBytesPerSec != null)
      dot(this.#yL(info.downBytesPerSec), this.#colors.download);
    if (info.upBytesPerSec != null)
      dot(this.#yL(info.upBytesPerSec), this.#colors.upload);
    if (info.rtt != null && info.latencyX != null) {
      ctx.fillStyle = this.#colors.warn;
      ctx.beginPath();
      const overflowGlyph = info.latencyOverflow
        ? latencyOverflowGlyph(this.#layout.plot.top)
        : null;
      const latencyY =
        overflowGlyph && info.rtt >= this.#vp.rttMax
          ? overflowGlyph.dot.y
          : this.#yR(info.rtt);
      ctx.arc(info.latencyX, latencyY, 2.5, 0, Math.PI * 2);
      ctx.fill();
    }
  }
}
