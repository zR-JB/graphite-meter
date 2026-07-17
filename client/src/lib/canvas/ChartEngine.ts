// Dual-axis timeseries. It redraws only for data, camera, theme, size, or hover
// invalidations; the shared presentation scheduler owns the frame clock.

import type {
  Phase,
  ThroughputSample,
  LatencySample,
} from "../runner/contract";
import type { CanvasEngine } from "./contract";
import { niceCeil, niceDomain, sharedThroughputScale } from "../format";
import { interpolateAt, lowerBoundAt } from "./hoverInterp";
import { presentation, type PresentationHandle } from "./presentation";

export interface ChartData {
  throughput: ThroughputSample[];
  latency: LatencySample[];
  /** False when latency is fully disabled — suppresses the latency line and
   *  the right (latency) axis so the chart reads as throughput-only. */
  latencyEnabled: boolean;
  phase: Phase;
  /** Exact phase boundary on the runner's measured timeline. */
  phaseStartedAtMs: number;
  /** Monotonic run counter from the store; a change means a new run started
   *  and the engine must drop all accumulated per-run state. */
  runSeq: number;
  /** Absolute throughput Y-axis ceiling (bytes/s), shared verbatim with the gauge
   *  dial (store.displayScaleBytesPerSec) so the two instruments are identically
   *  scaled. Already dwell-filtered + tiered upstream; the chart just follows it. */
  scaleBytesPerSec: number;
  /** Canonical headline rates produced by the measurement reducer. */
  resultRates: Partial<
    Record<"download" | "upload" | "bidiDown" | "bidiUp", number>
  >;
}

export interface ChartFormatters {
  throughput: (bytesPerSec: number) => string;
  latency: (rtt: number) => string;
}

export interface HoverInfo {
  x: number; // clamped css px within plot
  t: number; // ms
  /** The single lane's rate during download/upload; null during bidirectional
   *  (which reports its two lanes separately below) or where there's no
   *  throughput data under the cursor at all. */
  bytesPerSec: number | null;
  /** Bidirectional's two concurrent lanes; null outside that phase. */
  downBytesPerSec: number | null;
  upBytesPerSec: number | null;
  rtt: number | null;
}

interface Viewport {
  tMin: number;
  tMax: number;
  bytesPerSecMax: number;
  rttMin: number; // latency axis floor (0 live; centered span in result mode)
  rttMax: number;
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

const PAD_L = 46;
const PAD_R = 46;
const PAD_T = 12;
const PAD_B = 18;

interface ThemeColors {
  download: string;
  downloadRgb: { r: number; g: number; b: number };
  upload: string;
  uploadRgb: { r: number; g: number; b: number };
  bidirectional: string;
  warmup: string;
  signal: string;
  warn: string;
  warnSoft: string;
  grid: string;
  gridMajor: string;
  textSoft: string;
  text: string;
  panel: string;
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

/** Hex (#rgb/#rrggbb) → rgba() string at the given alpha. */
function withAlpha(hex: string, a: number): string {
  const { r, g, b } = hexToRgb(hex);
  return `rgba(${r},${g},${b},${a})`;
}

export class ChartEngine implements CanvasEngine {
  #get: () => ChartData;
  #fmt: ChartFormatters;
  #canvas: HTMLCanvasElement | null = null;
  #ctx: CanvasRenderingContext2D | null = null;
  #scene: HTMLCanvasElement | null = null;
  #sceneCtx: CanvasRenderingContext2D | null = null;
  #sceneDirty = true;
  #presentation: PresentationHandle | null = null;

  #dpr = 1;
  #w = 0;
  #h = 0;

  #vp: Viewport = {
    tMin: 0,
    tMax: 6000,
    bytesPerSecMax: 125_000,
    rttMin: 0,
    rttMax: 50,
  };
  #vpInit = false;

  // Rebuilt only when theme or plot height changes.
  #gradDownload: CanvasGradient | null = null;
  #gradUpload: CanvasGradient | null = null;
  #gradH = -1;
  // Samples own data attribution; spans retain sample-free warmup boundaries.
  #spans: PhaseSpan[] = [];
  #lastPhase: Phase | null = null;
  #hoverX: number | null = null;
  #result = false; // frozen post-run result mode
  #runSeq = -1; // last-seen store.runSeq; a change triggers a full reset
  #hasThroughputScale = false;
  #p95Cache = { len: -1, tMin: 0, v: 0 };
  #indexedThroughput = 0;
  #lastIndexedThroughput: ThroughputSample | undefined;
  #indexedLatency = 0;
  #lastIndexedLatency: LatencySample | undefined;
  #throughputByLane: Record<ThroughputLane, ThroughputSample[]> = {
    download: [],
    upload: [],
    bidiDown: [],
    bidiUp: [],
  };
  #latencyByPhase = new Map<Phase, LatencySample[]>();

  #c: ThemeColors = {
    download: "#6db0b8",
    downloadRgb: { r: 109, g: 176, b: 184 },
    upload: "#bda36c",
    uploadRgb: { r: 189, g: 163, b: 108 },
    bidirectional: "#a695c8",
    warmup: "#858c94",
    signal: "#8ba3ba",
    warn: "#c4a568",
    warnSoft: "rgba(196,165,104,0.14)",
    grid: "rgba(211,219,227,0.05)",
    gridMajor: "rgba(211,219,227,0.1)",
    textSoft: "#6a717a",
    text: "#d9dce0",
    panel: "#1c1f23",
    brand: "#6db0b8",
  };

  constructor(get: () => ChartData, fmt: ChartFormatters) {
    this.#get = get;
    this.#fmt = fmt;
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
    // Cap at 2 — same rationale as the gauge: raster cost, not fidelity.
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
    const plotW = this.#w - PAD_L - PAD_R;
    if (plotW <= 0) return null;
    const x = Math.max(PAD_L, Math.min(this.#w - PAD_R, this.#hoverX));
    const frac = (x - PAD_L) / plotW;
    const t = this.#vp.tMin + frac * (this.#vp.tMax - this.#vp.tMin);
    const data = this.#get();
    this.#indexData(data);
    const bytesPerSec =
      interpolateAt(this.#throughputByLane.download, t, (s) => s.bytesPerSec) ??
      interpolateAt(this.#throughputByLane.upload, t, (s) => s.bytesPerSec);
    const downBytesPerSec = interpolateAt(
      this.#throughputByLane.bidiDown,
      t,
      (s) => s.bytesPerSec,
    );
    const upBytesPerSec = interpolateAt(
      this.#throughputByLane.bidiUp,
      t,
      (s) => s.bytesPerSec,
    );
    let rtt: number | null = null;
    for (const lane of this.#latencyByPhase.values()) {
      rtt = interpolateAt(lane, t, (s) => s.rttMs);
      if (rtt != null) break;
    }
    if (
      bytesPerSec == null &&
      downBytesPerSec == null &&
      upBytesPerSec == null &&
      rtt == null
    )
      return null;
    return {
      x,
      t,
      bytesPerSec,
      downBytesPerSec,
      upBytesPerSec,
      rtt,
    };
  }

  #resolveColors(): void {
    const cs = getComputedStyle(document.documentElement);
    const g = (v: string, fb: string) => cs.getPropertyValue(v).trim() || fb;
    const download = g("--phase-download", "#6db0b8");
    const upload = g("--phase-upload", "#bda36c");
    this.#c = {
      download,
      downloadRgb: hexToRgb(download),
      upload,
      uploadRgb: hexToRgb(upload),
      bidirectional: g("--phase-bidirectional", "#a695c8"),
      warmup: g("--phase-warmup", "#858c94"),
      signal: g("--signal", "#8ba3ba"),
      warn: g("--warn", "#c4a568"),
      warnSoft: g("--warn-soft", "rgba(196,165,104,0.14)"),
      grid: g("--grid-line", "rgba(211,219,227,0.05)"),
      gridMajor: g("--grid-line-major", "rgba(211,219,227,0.1)"),
      textSoft: g("--text-soft", "#6a717a"),
      text: g("--text", "#d9dce0"),
      panel: g("--surface-1", "#1c1f23"),
      brand: g("--brand", "#6db0b8"),
    };
    this.#gradH = -1; // colors changed → rebuild cached gradients on next draw
  }

  #areaGrad(
    ctx: CanvasRenderingContext2D,
    phase: "download" | "upload",
  ): CanvasGradient {
    if (this.#gradH !== this.#h) {
      const bot = this.#h - PAD_B;
      const make = (rgb: {
        r: number;
        g: number;
        b: number;
      }): CanvasGradient => {
        const grad = ctx.createLinearGradient(0, PAD_T, 0, bot);
        grad.addColorStop(0, `rgba(${rgb.r},${rgb.g},${rgb.b},0.22)`);
        grad.addColorStop(1, `rgba(${rgb.r},${rgb.g},${rgb.b},0)`);
        return grad;
      };
      this.#gradDownload = make(this.#c.downloadRgb);
      this.#gradUpload = make(this.#c.uploadRgb);
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
    this.#p95Cache = { len: -1, tMin: 0, v: 0 };
    this.#indexedThroughput = 0;
    this.#lastIndexedThroughput = undefined;
    this.#indexedLatency = 0;
    this.#lastIndexedLatency = undefined;
    for (const lane of Object.values(this.#throughputByLane)) lane.length = 0;
    this.#latencyByPhase.clear();
  }

  #update(): void {
    const d = this.#get();

    if (d.runSeq !== this.#runSeq) {
      this.#runSeq = d.runSeq;
      this.#resetRunState();
    }
    this.#indexData(d);

    // Track exact runner-owned boundaries. Sample timestamps cannot represent
    // a sample-free warmup and therefore are not a phase clock.
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

    let rttMin = 0;
    let rttMax: number;
    if (complete) {
      const rtts: number[] = [];
      for (const s of d.latency) if (!s.lost) rtts.push(s.rttMs);
      const dom = niceDomain(rtts, { floor: 1 });
      rttMin = dom.min;
      rttMax = dom.max;
    } else {
      if (
        d.latency.length !== this.#p95Cache.len ||
        Math.abs(tMin - this.#p95Cache.tMin) > 500
      ) {
        this.#p95Cache = {
          len: d.latency.length,
          tMin,
          v: this.#p95In(d.latency, tMin, tMax),
        };
      }
      rttMax = niceCeil(Math.max(this.#p95Cache.v * 1.3, 20));
    }

    const target: Viewport = { tMin, tMax, bytesPerSecMax, rttMin, rttMax };
    if (!this.#vpInit) {
      this.#vp = { ...target };
      this.#vpInit = true;
    } else {
      this.#vp = target;
    }
  }

  #p95In(arr: LatencySample[], t0: number, t1: number): number {
    const v: number[] = [];
    const lo = lowerBoundAt(arr, t0);
    const hi = lowerBoundAt(arr, t1);
    for (let i = lo; i < hi; i++) if (!arr[i].lost) v.push(arr[i].rttMs);
    if (!v.length) return 0;
    v.sort((a, b) => a - b);
    return v[Math.min(v.length - 1, Math.ceil(0.95 * v.length) - 1)];
  }

  /* ---------- coordinate maps ---------- */
  #x(t: number): number {
    const plotW = this.#w - PAD_L - PAD_R;
    return (
      PAD_L + ((t - this.#vp.tMin) / (this.#vp.tMax - this.#vp.tMin)) * plotW
    );
  }
  #yL(bytesPerSec: number): number {
    const plotH = this.#h - PAD_T - PAD_B;
    return PAD_T + (1 - bytesPerSec / this.#vp.bytesPerSecMax) * plotH;
  }
  #yR(rtt: number): number {
    const plotH = this.#h - PAD_T - PAD_B;
    const span = this.#vp.rttMax - this.#vp.rttMin || 1;
    return PAD_T + (1 - (rtt - this.#vp.rttMin) / span) * plotH;
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
    this.#drawAxesLabels(ctx, d.latencyEnabled);
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

  /** Phase colour for the ribbon / labels (null = not shown). Warmup is a
   *  muted grey so it recedes and never reads like the (lavender) upload. */
  #phaseColor(phase: Phase): string | null {
    if (phase === "warmup") return this.#c.textSoft;
    if (phase === "latency") return this.#c.signal;
    if (phase === "download") return this.#c.download;
    if (phase === "upload") return this.#c.upload;
    if (phase === "bidirectional") return this.#c.bidirectional;
    return null;
  }

  #drawPhaseStats(
    ctx: CanvasRenderingContext2D,
    all: ThroughputSample[],
  ): void {
    for (const stat of this.#phaseStats(all)) {
      const x0 = Math.max(PAD_L, this.#x(stat.t0));
      const x1 = Math.min(this.#w - PAD_R, this.#x(stat.t1));
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

      const label = `avg ${this.#fmt.throughput(stat.avg)}`;
      ctx.font = '700 9px "JetBrains Mono", monospace';
      ctx.textAlign = "left";
      ctx.textBaseline = "alphabetic";
      const padX = 5;
      const chipH = 14;
      const tw = ctx.measureText(label).width;
      const chipW = tw + padX * 2;
      const chipX = Math.min(x0 + 3, this.#w - PAD_R - chipW);
      let chipY = yAvg - 4 - chipH;
      if (chipY < PAD_T) chipY = yAvg + 4;
      const baselineY = chipY + chipH - 4;

      ctx.beginPath();
      ctx.roundRect(chipX, chipY, chipW, chipH, 4);
      ctx.fillStyle = this.#c.panel;
      ctx.fill();
      ctx.lineWidth = 1;
      ctx.strokeStyle = withAlpha(stat.stroke, 0.55);
      ctx.stroke();

      ctx.fillStyle = stat.stroke;
      ctx.fillText(label, chipX + padX, baselineY);
    }
  }

  #phaseStats(all: ThroughputSample[]): PhaseStat[] {
    const out: PhaseStat[] = [];
    for (const phase of ["download", "upload"] as const) {
      const seg = all.filter((s) => s.phase === phase);
      const average = this.#get().resultRates[phase];
      if (seg.length < 2 || average == null) continue;
      out.push(
        this.#reduceStat(
          seg,
          phase === "download" ? this.#c.download : this.#c.upload,
          average,
        ),
      );
    }
    for (const dir of ["down", "up"] as const) {
      const seg = all.filter(
        (s) => s.phase === "bidirectional" && s.dir === dir,
      );
      const average =
        this.#get().resultRates[dir === "down" ? "bidiDown" : "bidiUp"];
      if (seg.length < 2 || average == null) continue;
      out.push(
        this.#reduceStat(
          seg,
          dir === "down" ? this.#c.download : this.#c.upload,
          average,
        ),
      );
    }
    return out;
  }

  #reduceStat(
    seg: ThroughputSample[],
    stroke: string,
    canonicalAverage: number,
  ): PhaseStat {
    return {
      t0: seg[0].t,
      t1: seg[seg.length - 1].t,
      avg: canonicalAverage,
      stroke,
    };
  }

  #PHASE_NAME: Partial<Record<Phase, string>> = {
    warmup: "WARM-UP",
    latency: "PING",
    download: "DOWNLOAD",
    upload: "UPLOAD",
    bidirectional: "BI-DIR",
  };

  #drawPhases(ctx: CanvasRenderingContext2D): void {
    const ry = this.#h - PAD_B + 4;
    let warmupLabelled = false;
    for (const s of this.#spans) {
      const color = this.#phaseColor(s.phase);
      if (!color) continue;
      const x0 = Math.max(PAD_L, this.#x(s.t0));
      const x1 = Math.min(
        this.#w - PAD_R,
        this.#x(s.t1 === Infinity ? this.#vp.tMax : s.t1),
      );
      const w = x1 - x0 - 2;
      if (w < 3) continue;

      ctx.globalAlpha = 0.85;
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.roundRect(x0, ry, w, 3, 1.5);
      ctx.fill();

      const isRepeatWarmup = s.phase === "warmup" && warmupLabelled;
      if (this.#result && w > 56 && !isRepeatWarmup) {
        ctx.globalAlpha = 0.62;
        ctx.font = '700 9px "JetBrains Mono", monospace';
        ctx.textAlign = "left";
        ctx.textBaseline = "alphabetic";
        ctx.fillText(this.#PHASE_NAME[s.phase] ?? "", x0 + 3, PAD_T + 9);
      }
      if (s.phase === "warmup") warmupLabelled = true;
    }
    ctx.globalAlpha = 1;
  }

  /** Adaptive nice step (ms) targeting ~5 vertical divisions for any span. */
  #niceTimeStep(target: number): number {
    const steps = [1000, 2000, 5000, 10000, 20000, 30000, 60000];
    for (const s of steps) if (s >= target) return s;
    return 60000;
  }

  #drawGrid(ctx: CanvasRenderingContext2D): void {
    const top = PAD_T;
    const bot = this.#h - PAD_B;
    const left = PAD_L;
    const right = this.#w - PAD_R;
    const step = this.#niceTimeStep((this.#vp.tMax - this.#vp.tMin) / 5);
    const startT = Math.ceil(this.#vp.tMin / step) * step;

    ctx.lineWidth = 1;
    ctx.strokeStyle = this.#c.grid;
    ctx.globalAlpha = 0.55;
    const minorStep = step / 4;
    const minorStartT = Math.ceil(this.#vp.tMin / minorStep) * minorStep;
    ctx.beginPath();
    for (let t = minorStartT; t <= this.#vp.tMax; t += minorStep) {
      const x = Math.round(this.#x(t)) + 0.5;
      if (x < left || x > right) continue;
      ctx.moveTo(x, top);
      ctx.lineTo(x, bot);
    }
    for (let i = 1; i < 16; i++) {
      if (i % 4 === 0) continue; // major line, drawn below
      const y = Math.round(top + ((bot - top) * i) / 16) + 0.5;
      ctx.moveTo(left, y);
      ctx.lineTo(right, y);
    }
    ctx.stroke();
    ctx.globalAlpha = 1;

    ctx.strokeStyle = this.#c.grid;
    ctx.fillStyle = this.#c.textSoft;
    ctx.font = '10px "JetBrains Mono", monospace';
    ctx.textAlign = "center";
    ctx.beginPath();
    for (let t = startT; t <= this.#vp.tMax; t += step) {
      const x = Math.round(this.#x(t)) + 0.5;
      if (x < left || x > right) continue;
      ctx.moveTo(x, top);
      ctx.lineTo(x, bot);
      const s = t / 1000;
      ctx.fillText(
        Number.isInteger(s) ? `${s}s` : `${s.toFixed(1)}s`,
        x,
        this.#h - 5,
      );
    }
    for (let i = 1; i <= 3; i++) {
      const y = Math.round(top + ((bot - top) * i) / 4) + 0.5;
      ctx.moveTo(left, y);
      ctx.lineTo(right, y);
    }
    ctx.stroke();
  }

  #smoothTo(
    ctx: CanvasRenderingContext2D,
    pts: { x: number; y: number }[],
  ): void {
    for (let i = 0; i < pts.length - 1; i++) {
      const xc = (pts[i].x + pts[i + 1].x) / 2;
      const yc = (pts[i].y + pts[i + 1].y) / 2;
      ctx.quadraticCurveTo(pts[i].x, pts[i].y, xc, yc);
    }
    const last = pts[pts.length - 1];
    ctx.lineTo(last.x, last.y);
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

    const latency = data.latency;
    if (
      latency.length < this.#indexedLatency ||
      (latency.length === this.#indexedLatency &&
        latency.at(-1) !== this.#lastIndexedLatency)
    ) {
      this.#indexedLatency = 0;
      this.#latencyByPhase.clear();
    }
    for (let i = this.#indexedLatency; i < latency.length; i++) {
      const sample = latency[i];
      if (sample.lost) continue;
      const lane = this.#latencyByPhase.get(sample.phase) ?? [];
      if (!lane.length) this.#latencyByPhase.set(sample.phase, lane);
      lane.push(sample);
    }
    this.#indexedLatency = latency.length;
    this.#lastIndexedLatency = latency.at(-1);
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
    const bot = this.#h - PAD_B;
    const tMin = this.#vp.tMin;
    const tMax = this.#vp.tMax;
    for (const lane of this.#throughputLanes()) {
      const stroke =
        lane.area === "download" ? this.#c.download : this.#c.upload;
      const pts: { x: number; y: number }[] = [];
      const lo = Math.max(0, lowerBoundAt(lane.samples, tMin) - 1);
      const hi = Math.min(
        lane.samples.length,
        lowerBoundAt(lane.samples, tMax) + 1,
      );
      for (let i = lo; i < hi; i++) {
        const s = lane.samples[i];
        pts.push({ x: this.#x(s.t), y: this.#yL(s.bytesPerSec) });
      }
      if (pts.length < 2) continue;

      ctx.fillStyle = this.#areaGrad(ctx, lane.area);
      ctx.beginPath();
      ctx.moveTo(pts[0].x, bot);
      ctx.lineTo(pts[0].x, pts[0].y);
      this.#smoothTo(ctx, pts);
      ctx.lineTo(pts[pts.length - 1].x, bot);
      ctx.closePath();
      ctx.fill();

      ctx.strokeStyle = stroke;
      ctx.lineWidth = 1.75;
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      ctx.beginPath();
      ctx.moveTo(pts[0].x, pts[0].y);
      this.#smoothTo(ctx, pts);
      ctx.stroke();
    }
  }

  #drawLatency(ctx: CanvasRenderingContext2D, all: LatencySample[]): void {
    if (all.length < 2) return;
    ctx.lineWidth = 1;
    const lo = Math.max(0, lowerBoundAt(all, this.#vp.tMin) - 1);
    const hi = Math.min(all.length, lowerBoundAt(all, this.#vp.tMax) + 1);
    let prevPhase: Phase | null = null;
    const segment: Array<{ t: number; rttMs: number; underLoad: boolean }> = [];
    const drawSegment = (): void => {
      if (segment.length < 2) {
        segment.length = 0;
        return;
      }
      const pts = segment.map((p) => ({
        x: this.#x(p.t),
        y: this.#yR(p.rttMs),
      }));
      ctx.strokeStyle = segment[0].underLoad ? this.#c.warn : this.#c.signal;
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      ctx.beginPath();
      ctx.moveTo(pts[0].x, pts[0].y);
      this.#smoothTo(ctx, pts);
      ctx.stroke();
      segment.length = 0;
    };
    for (let i = lo; i < hi; i++) {
      const s = all[i];
      if (
        s.lost ||
        (prevPhase !== null && s.phase !== prevPhase) ||
        (segment.length > 0 &&
          s.underLoad !== segment[segment.length - 1].underLoad)
      ) {
        drawSegment();
        prevPhase = null;
      }
      if (s.lost) {
        continue;
      }
      segment.push({ t: s.t, rttMs: s.rttMs, underLoad: s.underLoad });
      prevPhase = s.phase;
    }
    drawSegment();
  }

  #drawAxesLabels(
    ctx: CanvasRenderingContext2D,
    latencyEnabled: boolean,
  ): void {
    const top = PAD_T;
    const bot = this.#h - PAD_B;
    ctx.font = '10px "JetBrains Mono", monospace';
    ctx.fillStyle = this.#c.textSoft;
    // Left: throughput. Right: latency (omitted when latency is disabled).
    for (let i = 0; i <= 2; i++) {
      const frac = i / 2; // 0 top, 1 bottom
      const y = top + (bot - top) * frac;
      const bytesPerSec = this.#vp.bytesPerSecMax * (1 - frac);
      ctx.textAlign = "left";
      if (this.#hasThroughputScale) {
        ctx.fillText(this.#fmt.throughput(bytesPerSec), 4, y + 3);
      }
      if (latencyEnabled) {
        const rtt =
          this.#vp.rttMin + (this.#vp.rttMax - this.#vp.rttMin) * (1 - frac);
        ctx.textAlign = "right";
        ctx.fillText(this.#fmt.latency(rtt), this.#w - 4, y + 3);
      }
    }
  }

  #drawHover(ctx: CanvasRenderingContext2D): void {
    if (this.#hoverX == null) return;
    const x = Math.max(PAD_L, Math.min(this.#w - PAD_R, this.#hoverX));
    const top = PAD_T;
    const bot = this.#h - PAD_B;
    ctx.strokeStyle = this.#c.brand;
    ctx.lineWidth = 1;
    const gx = Math.round(x) + 0.5;
    ctx.beginPath();
    ctx.moveTo(gx, top);
    ctx.lineTo(gx, bot);
    ctx.stroke();

    const info = this.hoverInfo();
    if (!info) return;
    // Dots ride the interpolated value (matches the chip + the drawn line).
    if (info.bytesPerSec != null) {
      ctx.fillStyle = this.#c.brand;
      ctx.beginPath();
      ctx.arc(x, this.#yL(info.bytesPerSec), 2.5, 0, Math.PI * 2);
      ctx.fill();
    }
    // Bidirectional: one dot per lane, tinted to match its drawn line.
    if (info.downBytesPerSec != null) {
      ctx.fillStyle = this.#c.download;
      ctx.beginPath();
      ctx.arc(x, this.#yL(info.downBytesPerSec), 2.5, 0, Math.PI * 2);
      ctx.fill();
    }
    if (info.upBytesPerSec != null) {
      ctx.fillStyle = this.#c.upload;
      ctx.beginPath();
      ctx.arc(x, this.#yL(info.upBytesPerSec), 2.5, 0, Math.PI * 2);
      ctx.fill();
    }
    if (info.rtt != null) {
      ctx.fillStyle = this.#c.warn;
      ctx.beginPath();
      ctx.arc(x, this.#yR(info.rtt), 2.5, 0, Math.PI * 2);
      ctx.fill();
    }
  }
}
