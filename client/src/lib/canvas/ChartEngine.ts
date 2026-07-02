/* ============================================================
 * The Graphite Meter — ChartEngine (§3.2)
 * Dual-axis timeseries: throughput area (left axis, phase-tinted)
 * + latency line (right axis, signal/warn-for-loaded). Live
 * scrolling viewport, eased zoom-out on complete, time grid,
 * bufferbloat shading, and hover scrub. Pulls store ring buffers
 * each frame via its own rAF loop.
 * ============================================================ */

import type { Phase, ThroughputSample, LatencySample } from "../runner/contract";
import type { CanvasEngine } from "./contract";
import { niceCeil, niceDomain, sharedThroughputScale } from "../format";

export interface ChartData {
  throughput: ThroughputSample[];
  latency: LatencySample[];
  /** False when latency is fully disabled — suppresses the latency line and
   *  the right (latency) axis so the chart reads as throughput-only. */
  latencyEnabled: boolean;
  phase: Phase;
  /** Monotonic run counter from the store; a change means a new run started
   *  and the engine must drop all accumulated per-run state. */
  runSeq: number;
  /** Absolute throughput Y-axis ceiling (bytes/s), shared verbatim with the gauge
   *  dial (store.displayScaleBytesPerSec) so the two instruments are identically
   *  scaled. Already dwell-filtered + tiered upstream; the chart just follows it. */
  scaleBytesPerSec: number;
}

export interface ChartFormatters {
  throughput: (bytesPerSec: number) => string;
  latency: (rtt: number) => string;
}

export interface HoverInfo {
  x: number; // clamped css px within plot
  t: number; // ms
  bytesPerSec: number | null;
  rtt: number | null;
}

interface Viewport {
  tMin: number;
  tMax: number;
  bytesPerSecMax: number;
  rttMin: number; // latency axis floor (0 live; centered span in result mode)
  rttMax: number;
}

/** Per-phase throughput summary drawn as min/max/avg overlays in result mode. */
interface PhaseStat {
  phase: Phase;
  t0: number;
  t1: number;
  min: number;
  max: number;
  avg: number;
  stroke: string;
}

interface PhaseSpan {
  phase: Phase;
  t0: number;
  t1: number; // Infinity while open
}

const PAD_L = 46;
const PAD_R = 46;
const PAD_T = 12;
const PAD_B = 18;
const FOLLOW = 0.18; // viewport lerp factor

interface ThemeColors {
  download: string;
  downloadRgb: { r: number; g: number; b: number };
  upload: string;
  uploadRgb: { r: number; g: number; b: number };
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
  const full = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
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
  #raf = 0;
  #running = false;

  #dpr = 1;
  #w = 0;
  #h = 0;

  #vp: Viewport = { tMin: 0, tMax: 6000, bytesPerSecMax: 125_000, rttMin: 0, rttMax: 50 };
  #vpInit = false;
  #vpSettled = false; // true once the camera lerp has converged on its target

  // Cached area-fill gradients. They depend only on the phase color + plot
  // height, so they're rebuilt only when the height changes or the theme is
  // re-resolved (which resets #gradH) — never per frame.
  #gradDownload: CanvasGradient | null = null;
  #gradUpload: CanvasGradient | null = null;
  #gradH = -1;
  // Phase timeline, tracked from phase-change events. COSMETIC ONLY: it drives
  // the bottom phase ribbon and the live camera's start edge. Data attribution
  // (throughput area + per-phase stats) keys off each sample's own `phase` tag,
  // NOT this — the ribbon still needs a transition timeline because sample-less
  // phases (warmup) produce no tagged samples to reconstruct it from.
  #spans: PhaseSpan[] = [];
  #lastPhase: Phase | null = null;
  #hoverX: number | null = null;
  #result = false; // frozen post-run result mode
  #runSeq = -1; // last-seen store.runSeq; a change triggers a full reset
  #hasThroughputScale = false;
  #p95Cache = { len: -1, tMin: 0, v: 0 };

  #c: ThemeColors = {
    download: "#6db0b8",
    downloadRgb: { r: 109, g: 176, b: 184 },
    upload: "#bda36c",
    uploadRgb: { r: 189, g: 163, b: 108 },
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
    this.invalidateTheme();
  }

  start(): void {
    this.wake();
  }

  /** Re-arm the loop if it has parked. No-op while already running. */
  wake(): void {
    if (this.#running || !this.#ctx) return;
    this.#running = true;
    this.#raf = requestAnimationFrame(this.#loop);
  }

  stop(): void {
    this.#running = false;
    if (this.#raf) cancelAnimationFrame(this.#raf);
    this.#raf = 0;
  }

  destroy(): void {
    this.stop();
    this.#canvas = null;
    this.#ctx = null;
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
    this.#resolveColors();
    // Resizing the backing store wipes it — repaint NOW (the ResizeObserver
    // fires between layout and paint) so the chart never blanks mid-resize.
    this.#draw();
  }

  setHover(x: number | null): void {
    this.#hoverX = x;
    this.wake(); // the loop may be parked (idle/result) — redraw the guideline
  }

  /** Hover readout under the cursor (for the DOM chip). Snaps to the time
   *  under the pointer, then LINEARLY INTERPOLATES the series value between
   *  the two bracketing samples (not nearest-only) — ports linerate's
   *  `transferPointAt`/`latencyPointAt` weight-blend onto the time axis. */
  hoverInfo(): HoverInfo | null {
    if (this.#hoverX == null) return null;
    const plotW = this.#w - PAD_L - PAD_R;
    if (plotW <= 0) return null;
    const x = Math.max(PAD_L, Math.min(this.#w - PAD_R, this.#hoverX));
    const frac = (x - PAD_L) / plotW;
    const t = this.#vp.tMin + frac * (this.#vp.tMax - this.#vp.tMin);
    const data = this.#get();
    const bytesPerSec = this.#hoverValue(data.throughput, t, (s) => s.bytesPerSec);
    const rtt = this.#hoverValue(
      data.latency.filter((s) => !s.lost),
      t,
      (s) => s.rttMs,
    );
    if (bytesPerSec == null && rtt == null) return null;
    return {
      x,
      t,
      bytesPerSec,
      rtt,
    };
  }

  #hoverValue<T extends { t: number; phase: Phase }>(
    arr: T[],
    t: number,
    pick: (s: T) => number,
  ): number | null {
    if (!arr.length) return null;
    const span = this.#spanAt(t);
    if (span) {
      const phaseSamples = arr.filter((s) => s.phase === span.phase);
      const value = this.#interpInRange(phaseSamples, t, pick);
      if (value != null) return value;
      return null;
    }

    const nearestPhase = this.#nearestPhaseWithSamples(arr, t);
    if (nearestPhase) {
      const firstSample = arr.find((s) => s.phase === nearestPhase);
      if (firstSample) return pick(firstSample);
    }

    return pick(this.#closestSample(arr, t));
  }

  #interpInRange<T extends { t: number }>(arr: T[], t: number, pick: (s: T) => number): number | null {
    if (!arr.length) return null;
    if (t < arr[0].t || t > arr[arr.length - 1].t) return null;
    if (t === arr[0].t) return pick(arr[0]);
    const last = arr[arr.length - 1];
    if (t === last.t) return pick(last);
    for (let i = 1; i < arr.length; i++) {
      const b = arr[i];
      if (b.t >= t) {
        const a = arr[i - 1];
        const span = b.t - a.t || 1;
        const w = (t - a.t) / span;
        return pick(a) * (1 - w) + pick(b) * w;
      }
    }
    return null;
  }

  #spanAt(t: number): PhaseSpan | null {
    for (const span of this.#spans) {
      if (t >= span.t0 && t <= span.t1) return span;
    }
    return null;
  }

  #nearestPhaseWithSamples<T extends { phase: Phase }>(arr: T[], t: number): Phase | null {
    if (!this.#spans.length) return null;
    const availablePhases = new Set(arr.map((s) => s.phase));
    let nearest: Phase | null = null;
    let nearestDist = Infinity;
    for (const span of this.#spans) {
      if (!availablePhases.has(span.phase)) continue;
      const dist = t < span.t0 ? span.t0 - t : t > span.t1 ? t - span.t1 : 0;
      if (dist < nearestDist) {
        nearestDist = dist;
        nearest = span.phase;
      }
    }
    return nearest;
  }

  #closestSample<T extends { t: number }>(arr: T[], t: number): T {
    let closest = arr[0];
    let closestDist = Math.abs(arr[0].t - t);
    for (let i = 1; i < arr.length; i++) {
      const sample = arr[i];
      const dist = Math.abs(sample.t - t);
      if (dist < closestDist) {
        closestDist = dist;
        closest = sample;
      }
    }
    return closest;
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

  /** Cached vertical area-fill gradient for a transfer phase. Rebuilt only when
   *  the plot height changes or the theme is re-resolved (#gradH reset). */
  #areaGrad(ctx: CanvasRenderingContext2D, phase: "download" | "upload"): CanvasGradient {
    if (this.#gradH !== this.#h) {
      const bot = this.#h - PAD_B;
      const make = (rgb: { r: number; g: number; b: number }): CanvasGradient => {
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

  #loop = (): void => {
    if (!this.#running) return;
    this.#update();
    this.#draw();
    if (this.#animating()) {
      this.#raf = requestAnimationFrame(this.#loop);
    } else {
      // Camera settled and no live phase feeding the chart — park so the GPU
      // can idle. setHover()/wake() re-arm it for hover scrub or a new run.
      this.#running = false;
      this.#raf = 0;
    }
  };

  /** True while the chart still has motion in flight. Live phases scroll the
   *  viewport continuously; idle/result phases animate only until the camera
   *  lerp settles, then the loop parks. */
  #animating(): boolean {
    const p = this.#lastPhase;
    if (p === "warmup" || p === "latency" || p === "download" || p === "upload") return true;
    return !this.#vpSettled;
  }

  #latestT(d: ChartData): number {
    const a = d.throughput.length ? d.throughput[d.throughput.length - 1].t : 0;
    const b = d.latency.length ? d.latency[d.latency.length - 1].t : 0;
    return Math.max(a, b);
  }

  /** Drop all accumulated per-run state. Called when the store's runSeq
   *  changes (reset/engage/return-home) so nothing from a prior run — phase
   *  spans, the settled camera, result overlays — can bleed into the next. */
  #resetRunState(): void {
    this.#spans = [];
    this.#lastPhase = null;
    this.#vpInit = false;
    this.#result = false;
    this.#hasThroughputScale = false;
    this.#p95Cache = { len: -1, tMin: 0, v: 0 };
  }

  #update(): void {
    const d = this.#get();

    // New run → clear everything accumulated from the previous one.
    if (d.runSeq !== this.#runSeq) {
      this.#runSeq = d.runSeq;
      this.#resetRunState();
    }

    // Track phase spans (for per-phase colouring + the phase ribbon).
    if (d.phase !== this.#lastPhase) {
      const lt = this.#latestT(d);
      if (this.#spans.length) this.#spans[this.#spans.length - 1].t1 = lt;
      this.#spans.push({ phase: d.phase, t0: lt, t1: Infinity });
      this.#lastPhase = d.phase;
    }

    const latest = this.#latestT(d);
    const complete = d.phase === "complete" || d.phase === "aborted" || d.phase === "error";
    this.#result = complete;

    // Target viewport.
    let tMin: number;
    let tMax: number;
    if (complete) {
      // Frozen result view: settle the camera to the whole timeline.
      tMin = 0;
      tMax = Math.max(latest * 1.02, 1000);
    } else {
      const span = this.#spans[this.#spans.length - 1];
      const phaseStart = span ? span.t0 : 0;
      tMin = phaseStart;
      tMax = Math.max(latest + 2000, phaseStart + 4000);
    }

    // Throughput axis ceiling: follow the gauge's shared scale verbatim so the
    // two instruments are identically scaled (dwell-filtered + tiered upstream).
    const bytesPerSecMax = d.scaleBytesPerSec > 0 ? d.scaleBytesPerSec : 125_000;
    this.#hasThroughputScale = d.scaleBytesPerSec !== sharedThroughputScale(0) || d.throughput.length > 0;

    // Latency axis. Live → simple 0-based nice ceiling (stable while scrolling).
    // Result → centered, weighted, nice-step domain (shared `niceDomain`), so
    // a flat latency band fills the lane instead of hugging the floor.
    let rttMin = 0;
    let rttMax: number;
    if (complete) {
      const rtts: number[] = [];
      for (const s of d.latency) if (!s.lost) rtts.push(s.rttMs);
      // floor:1 matches LatencyProfile — scale down to a 1 ms span on a fast
      // LAN/localhost rather than bottoming out at a 20 ms step.
      const dom = niceDomain(rtts, { floor: 1 });
      rttMin = dom.min;
      rttMax = dom.max;
    } else {
      // Cached: the sort in #p95In only reruns when a sample lands or the
      // window has drifted meaningfully — not on all 60 camera frames/s.
      if (
        d.latency.length !== this.#p95Cache.len ||
        Math.abs(tMin - this.#p95Cache.tMin) > 500
      ) {
        this.#p95Cache = { len: d.latency.length, tMin, v: this.#p95In(d.latency, tMin, tMax) };
      }
      rttMax = niceCeil(Math.max(this.#p95Cache.v * 1.3, 20));
    }

    const target: Viewport = { tMin, tMax, bytesPerSecMax, rttMin, rttMax };
    if (!this.#vpInit) {
      this.#vp = { ...target };
      this.#vpInit = true;
      this.#vpSettled = true;
    } else {
      // Lerp each axis toward target; snap (and flag unsettled) per-axis so the
      // loop knows when the camera has converged and can park. Epsilons are in
      // each axis's own units — sub-pixel for time, ~0.05% for the rate scale.
      let settled = true;
      const ease = (cur: number, tgt: number, eps: number): number => {
        const next = cur + (tgt - cur) * FOLLOW;
        if (Math.abs(tgt - next) <= eps) return tgt;
        settled = false;
        return next;
      };
      this.#vp.tMin = ease(this.#vp.tMin, target.tMin, 0.5);
      this.#vp.tMax = ease(this.#vp.tMax, target.tMax, 0.5);
      this.#vp.bytesPerSecMax = ease(
        this.#vp.bytesPerSecMax,
        target.bytesPerSecMax,
        Math.max(1, target.bytesPerSecMax * 0.0005),
      );
      this.#vp.rttMin = ease(this.#vp.rttMin, target.rttMin, 0.05);
      this.#vp.rttMax = ease(this.#vp.rttMax, target.rttMax, 0.05);
      this.#vpSettled = settled;
    }
  }


  #p95In(arr: LatencySample[], t0: number, t1: number): number {
    const v: number[] = [];
    for (const s of arr) if (!s.lost && s.t >= t0 && s.t <= t1) v.push(s.rttMs);
    if (!v.length) return 0;
    v.sort((a, b) => a - b);
    return v[Math.min(v.length - 1, Math.ceil(0.95 * v.length) - 1)];
  }

  /* ---------- coordinate maps ---------- */
  #x(t: number): number {
    const plotW = this.#w - PAD_L - PAD_R;
    return PAD_L + ((t - this.#vp.tMin) / (this.#vp.tMax - this.#vp.tMin)) * plotW;
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

  #draw(): void {
    const ctx = this.#ctx;
    if (!ctx) return;
    const d = this.#get();
    ctx.clearRect(0, 0, this.#w, this.#h);

    this.#drawGrid(ctx);
    this.#drawThroughput(ctx, d.throughput);
    if (this.#result) this.#drawPhaseStats(ctx, d.throughput);
    if (d.latencyEnabled) this.#drawLatency(ctx, d.latency);
    this.#drawPhases(ctx);
    this.#drawAxesLabels(ctx, d.latencyEnabled);
    this.#drawHover(ctx);
  }

  /** Phase colour for the ribbon / labels (null = not shown). Warmup is a
   *  muted grey so it recedes and never reads like the (lavender) upload. */
  #phaseColor(phase: Phase): string | null {
    if (phase === "warmup") return this.#c.textSoft;
    if (phase === "latency") return this.#c.signal;
    if (phase === "download") return this.#c.download;
    if (phase === "upload") return this.#c.upload;
    return null;
  }

  /** Per-phase throughput min/max/avg overlays — drawn only in the frozen
   *  result view. A faint min→max band per transfer phase plus a dashed
   *  average rule and a small "avg" tag. Ports linerate's per-series
   *  average-line + peak summary onto the canvas. */
  #drawPhaseStats(ctx: CanvasRenderingContext2D, all: ThroughputSample[]): void {
    for (const stat of this.#phaseStats(all)) {
      const x0 = Math.max(PAD_L, this.#x(stat.t0));
      const x1 = Math.min(this.#w - PAD_R, this.#x(stat.t1));
      if (x1 <= x0) continue;
      const yAvg = this.#yL(stat.avg);

      // Clean dashed average rule (no busy min→max band).
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

      // avg pill on a solid chip so the label never blends into the line it
      // sits on. Faceplate-styled: panel fill + hairline in the phase colour.
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
    // Group by the sample's own phase tag (not a re-derived time window) so the
    // per-phase stats attribute samples exactly as the engine reduces them.
    for (const phase of ["download", "upload"] as const) {
      const seg = all.filter((s) => s.phase === phase);
      if (seg.length < 2) continue;
      let min = Infinity;
      let max = 0;
      let sum = 0;
      for (const s of seg) {
        if (s.bytesPerSec < min) min = s.bytesPerSec;
        if (s.bytesPerSec > max) max = s.bytesPerSec;
        sum += s.bytesPerSec;
      }
      out.push({
        phase,
        t0: seg[0].t,
        t1: seg[seg.length - 1].t,
        min,
        max,
        avg: sum / seg.length,
        stroke: phase === "download" ? this.#c.download : this.#c.upload,
      });
    }
    return out;
  }

  /** Phase ribbon — a thin colour-coded strip in the bottom gutter mapping the
   *  timeline to its phases (colours match the throughput area), plus a small
   *  phase label per segment in the frozen result view. Replaces the old flat
   *  bufferbloat band: clear about what it shows, and on-brand. */
  #PHASE_NAME: Partial<Record<Phase, string>> = {
    warmup: "WARM-UP",
    latency: "PING",
    download: "DOWNLOAD",
    upload: "UPLOAD",
  };

  #drawPhases(ctx: CanvasRenderingContext2D): void {
    const ry = this.#h - PAD_B + 4;
    // Warmup recurs before every transfer stage but shares one colour, so we
    // label only the first warmup — repeating "WARM-UP" just adds clutter.
    let warmupLabelled = false;
    for (const s of this.#spans) {
      const color = this.#phaseColor(s.phase);
      if (!color) continue;
      const x0 = Math.max(PAD_L, this.#x(s.t0));
      const x1 = Math.min(this.#w - PAD_R, this.#x(s.t1 === Infinity ? this.#vp.tMax : s.t1));
      const w = x1 - x0 - 2;
      if (w < 3) continue;

      ctx.globalAlpha = 0.85;
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.roundRect(x0, ry, w, 3, 1.5);
      ctx.fill();

      // Name the segment in the static result view (room + no scroll).
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

    // Quad-ruled minor grid — a faint 4-division subdivision of each major
    // cell, underneath the labelled lines. Graph paper, not a chrome track.
    // All minor lines share one path/stroke (one raster pass, not ~40).
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

    // Labelled major lines — one batched path, labels filled alongside.
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
      ctx.fillText(Number.isInteger(s) ? `${s}s` : `${s.toFixed(1)}s`, x, this.#h - 5);
    }
    for (let i = 1; i <= 3; i++) {
      const y = Math.round(top + ((bot - top) * i) / 4) + 0.5;
      ctx.moveTo(left, y);
      ctx.lineTo(right, y);
    }
    ctx.stroke();
  }

  /** Trace a smoothed path through `pts` from the current point. Midpoint-
   *  quadratic: each sample is a control point and the curve passes through
   *  the segment midpoints — smooth, overshoot-free, cheap. */
  #smoothTo(ctx: CanvasRenderingContext2D, pts: { x: number; y: number }[]): void {
    for (let i = 0; i < pts.length - 1; i++) {
      const xc = (pts[i].x + pts[i + 1].x) / 2;
      const yc = (pts[i].y + pts[i + 1].y) / 2;
      ctx.quadraticCurveTo(pts[i].x, pts[i].y, xc, yc);
    }
    const last = pts[pts.length - 1];
    ctx.lineTo(last.x, last.y);
  }

  #drawThroughput(ctx: CanvasRenderingContext2D, all: ThroughputSample[]): void {
    if (!all.length) return;
    const bot = this.#h - PAD_B;
    const tMin = this.#vp.tMin;
    const tMax = this.#vp.tMax;
    // Group by the sample's phase tag (download/upload each run once, so each
    // yields a contiguous, time-ordered run). Samples outside the viewport are
    // culled, keeping one bridging sample per edge for path continuity — while
    // the live camera tracks the current phase, earlier phases cost nothing.
    for (const phase of ["download", "upload"] as const) {
      const stroke = phase === "download" ? this.#c.download : this.#c.upload;
      const pts: { x: number; y: number }[] = [];
      let leftEdge: ThroughputSample | null = null;
      for (const s of all) {
        if (s.phase !== phase) continue;
        if (s.t < tMin) {
          leftEdge = s;
          continue;
        }
        if (!pts.length && leftEdge) pts.push({ x: this.#x(leftEdge.t), y: this.#yL(leftEdge.bytesPerSec) });
        pts.push({ x: this.#x(s.t), y: this.#yL(s.bytesPerSec) });
        if (s.t > tMax) break;
      }
      if (pts.length < 2) continue;

      // Filled area under a smoothed top edge, soft vertical gradient (cached).
      ctx.fillStyle = this.#areaGrad(ctx, phase);
      ctx.beginPath();
      ctx.moveTo(pts[0].x, bot);
      ctx.lineTo(pts[0].x, pts[0].y);
      this.#smoothTo(ctx, pts);
      ctx.lineTo(pts[pts.length - 1].x, bot);
      ctx.closePath();
      ctx.fill();

      // Smoothed stroke on top.
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
    // Cull to the viewport (plus one bridging sample per edge) — the segment
    // logic below then only walks what is actually visible.
    let lo = 0;
    while (lo < all.length && all[lo].t < this.#vp.tMin) lo++;
    if (lo > 0) lo--;
    let hi = lo;
    while (hi < all.length && all[hi].t <= this.#vp.tMax) hi++;
    if (hi < all.length) hi++;
    // Use raw latency points and mild smoothing so the curve is continuous
    // without hiding the point measurements. Break on a lost ping, phase
    // boundary, or under-load colour transition.
    let prevPhase: Phase | null = null;
    const segment: Array<{ t: number; rttMs: number; underLoad: boolean }> = [];
    const drawSegment = (): void => {
      if (segment.length < 2) {
        segment.length = 0;
        return;
      }
      const pts = segment.map((p) => ({ x: this.#x(p.t), y: this.#yR(p.rttMs) }));
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
      if (s.lost || (prevPhase !== null && s.phase !== prevPhase) || (segment.length > 0 && s.underLoad !== segment[segment.length - 1].underLoad)) {
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

  #drawAxesLabels(ctx: CanvasRenderingContext2D, latencyEnabled: boolean): void {
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
        const rtt = this.#vp.rttMin + (this.#vp.rttMax - this.#vp.rttMin) * (1 - frac);
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
    ctx.fillStyle = this.#c.brand;
    if (info.bytesPerSec != null) {
      ctx.beginPath();
      ctx.arc(x, this.#yL(info.bytesPerSec), 2.5, 0, Math.PI * 2);
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
