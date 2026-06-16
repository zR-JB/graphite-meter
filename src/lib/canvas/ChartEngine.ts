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
import { niceCeil } from "../format";

export interface ChartData {
  throughput: ThroughputSample[];
  latency: LatencySample[];
  phase: Phase;
}

export interface ChartFormatters {
  throughput: (bps: number) => string;
  latency: (rtt: number) => string;
}

export interface HoverInfo {
  x: number; // clamped css px within plot
  t: number; // ms
  bps: number | null;
  rtt: number | null;
}

interface Viewport {
  tMin: number;
  tMax: number;
  bpsMax: number;
  rttMax: number;
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
  signal: string;
  warn: string;
  warnSoft: string;
  grid: string;
  gridMajor: string;
  textSoft: string;
  brand: string;
}

function hexToRgb(hex: string): { r: number; g: number; b: number } {
  const h = hex.replace("#", "").trim();
  const full = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  const n = parseInt(full || "888888", 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
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

  #vp: Viewport = { tMin: 0, tMax: 6000, bpsMax: 1e6, rttMax: 50 };
  #vpInit = false;
  #spans: PhaseSpan[] = [];
  #lastPhase: Phase | null = null;
  #hoverX: number | null = null;

  #c: ThemeColors = {
    download: "#6fa77a",
    downloadRgb: { r: 111, g: 167, b: 122 },
    upload: "#d7a84f",
    uploadRgb: { r: 215, g: 168, b: 79 },
    signal: "#7ea7a6",
    warn: "#d7a84f",
    warnSoft: "rgba(215,168,79,0.14)",
    grid: "rgba(255,255,255,0.05)",
    gridMajor: "rgba(255,255,255,0.09)",
    textSoft: "#737b76",
    brand: "#d7a84f",
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
    if (this.#running) return;
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
    this.#dpr = window.devicePixelRatio || 1;
    const rect = this.#canvas.getBoundingClientRect();
    this.#w = Math.max(1, rect.width);
    this.#h = Math.max(1, rect.height);
    this.#canvas.width = Math.round(this.#w * this.#dpr);
    this.#canvas.height = Math.round(this.#h * this.#dpr);
    this.#ctx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    this.#resolveColors();
  }

  setHover(x: number | null): void {
    this.#hoverX = x;
  }

  /** Nearest-sample readout under the hover cursor (for the DOM chip). */
  hoverInfo(): HoverInfo | null {
    if (this.#hoverX == null) return null;
    const plotW = this.#w - PAD_L - PAD_R;
    if (plotW <= 0) return null;
    const x = Math.max(PAD_L, Math.min(this.#w - PAD_R, this.#hoverX));
    const frac = (x - PAD_L) / plotW;
    const t = this.#vp.tMin + frac * (this.#vp.tMax - this.#vp.tMin);
    const data = this.#get();
    const bps = this.#nearest(data.throughput, t)?.bps ?? null;
    const lat = this.#nearest(data.latency, t);
    return { x, t, bps, rtt: lat && !lat.lost ? lat.rttMs : null };
  }

  #nearest<T extends { t: number }>(arr: T[], t: number): T | null {
    if (!arr.length) return null;
    let best = arr[0];
    let bestD = Math.abs(arr[0].t - t);
    for (let i = 1; i < arr.length; i++) {
      const d = Math.abs(arr[i].t - t);
      if (d < bestD) {
        bestD = d;
        best = arr[i];
      }
    }
    return best;
  }

  #resolveColors(): void {
    const cs = getComputedStyle(document.documentElement);
    const g = (v: string, fb: string) => cs.getPropertyValue(v).trim() || fb;
    const download = g("--phase-download", "#6fa77a");
    const upload = g("--phase-upload", "#d7a84f");
    this.#c = {
      download,
      downloadRgb: hexToRgb(download),
      upload,
      uploadRgb: hexToRgb(upload),
      signal: g("--signal", "#7ea7a6"),
      warn: g("--warn", "#d7a84f"),
      warnSoft: g("--warn-soft", "rgba(215,168,79,0.14)"),
      grid: g("--grid-line", "rgba(255,255,255,0.05)"),
      gridMajor: g("--grid-line-major", "rgba(255,255,255,0.09)"),
      textSoft: g("--text-soft", "#737b76"),
      brand: g("--brand", "#d7a84f"),
    };
  }

  #loop = (): void => {
    if (!this.#running) return;
    this.#update();
    this.#draw();
    this.#raf = requestAnimationFrame(this.#loop);
  };

  #latestT(d: ChartData): number {
    const a = d.throughput.length ? d.throughput[d.throughput.length - 1].t : 0;
    const b = d.latency.length ? d.latency[d.latency.length - 1].t : 0;
    return Math.max(a, b);
  }

  #update(): void {
    const d = this.#get();

    // Track phase spans (for per-phase colouring + bufferbloat band).
    if (d.phase !== this.#lastPhase) {
      const lt = this.#latestT(d);
      if (this.#spans.length) this.#spans[this.#spans.length - 1].t1 = lt;
      this.#spans.push({ phase: d.phase, t0: lt, t1: Infinity });
      this.#lastPhase = d.phase;
    }

    const latest = this.#latestT(d);
    const complete = d.phase === "complete" || d.phase === "aborted" || d.phase === "error";

    // Target viewport.
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

    const peak = this.#peakIn(d.throughput, tMin, tMax);
    const p95 = this.#p95In(d.latency, tMin, tMax);
    const bpsMax = niceCeil(Math.max(peak * 1.15, 1e6));
    const rttMax = niceCeil(Math.max(p95 * 1.3, 20));

    const target: Viewport = { tMin, tMax, bpsMax, rttMax };
    if (!this.#vpInit) {
      this.#vp = { ...target };
      this.#vpInit = true;
    } else {
      this.#vp.tMin += (target.tMin - this.#vp.tMin) * FOLLOW;
      this.#vp.tMax += (target.tMax - this.#vp.tMax) * FOLLOW;
      this.#vp.bpsMax += (target.bpsMax - this.#vp.bpsMax) * FOLLOW;
      this.#vp.rttMax += (target.rttMax - this.#vp.rttMax) * FOLLOW;
    }
  }

  #peakIn(arr: ThroughputSample[], t0: number, t1: number): number {
    let peak = 0;
    for (const s of arr) if (s.t >= t0 && s.t <= t1 && s.bps > peak) peak = s.bps;
    return peak;
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
  #yL(bps: number): number {
    const plotH = this.#h - PAD_T - PAD_B;
    return PAD_T + (1 - bps / this.#vp.bpsMax) * plotH;
  }
  #yR(rtt: number): number {
    const plotH = this.#h - PAD_T - PAD_B;
    return PAD_T + (1 - rtt / this.#vp.rttMax) * plotH;
  }

  #draw(): void {
    const ctx = this.#ctx;
    if (!ctx) return;
    const d = this.#get();
    ctx.clearRect(0, 0, this.#w, this.#h);

    this.#drawBufferbloatBands(ctx);
    this.#drawGrid(ctx);
    this.#drawThroughput(ctx, d.throughput);
    this.#drawLatency(ctx, d.latency);
    this.#drawAxesLabels(ctx);
    this.#drawHover(ctx, d);
  }

  #drawBufferbloatBands(ctx: CanvasRenderingContext2D): void {
    const top = PAD_T;
    const bot = this.#h - PAD_B;
    ctx.fillStyle = this.#c.warnSoft;
    for (const s of this.#spans) {
      if (s.phase !== "download" && s.phase !== "upload") continue;
      const x0 = Math.max(PAD_L, this.#x(s.t0));
      const x1 = Math.min(this.#w - PAD_R, this.#x(s.t1 === Infinity ? this.#vp.tMax : s.t1));
      if (x1 > x0) ctx.fillRect(x0, top, x1 - x0, bot - top);
    }
  }

  #drawGrid(ctx: CanvasRenderingContext2D): void {
    const top = PAD_T;
    const bot = this.#h - PAD_B;
    const startT = Math.ceil(this.#vp.tMin / 1000) * 1000;
    ctx.lineWidth = 1;
    for (let t = startT; t <= this.#vp.tMax; t += 1000) {
      const x = Math.round(this.#x(t)) + 0.5;
      if (x < PAD_L || x > this.#w - PAD_R) continue;
      const major = t % 5000 === 0;
      ctx.strokeStyle = major ? this.#c.gridMajor : this.#c.grid;
      ctx.beginPath();
      ctx.moveTo(x, top);
      ctx.lineTo(x, bot);
      ctx.stroke();
      if (major) {
        ctx.fillStyle = this.#c.textSoft;
        ctx.font = '10px "IBM Plex Mono", monospace';
        ctx.textAlign = "center";
        ctx.fillText(`${t / 1000}s`, x, this.#h - 5);
      }
    }
    // Horizontal throughput gridlines (left axis).
    ctx.strokeStyle = this.#c.grid;
    for (let i = 1; i <= 3; i++) {
      const y = Math.round(top + ((bot - top) * i) / 4) + 0.5;
      ctx.beginPath();
      ctx.moveTo(PAD_L, y);
      ctx.lineTo(this.#w - PAD_R, y);
      ctx.stroke();
    }
  }

  #drawThroughput(ctx: CanvasRenderingContext2D, all: ThroughputSample[]): void {
    if (!all.length) return;
    const bot = this.#h - PAD_B;
    for (const span of this.#spans) {
      if (span.phase !== "download" && span.phase !== "upload") continue;
      const t1 = span.t1 === Infinity ? this.#vp.tMax + 1 : span.t1;
      const seg = all.filter((s) => s.t >= span.t0 && s.t <= t1);
      if (seg.length < 2) continue;
      const rgb = span.phase === "download" ? this.#c.downloadRgb : this.#c.uploadRgb;
      const stroke = span.phase === "download" ? this.#c.download : this.#c.upload;

      // Filled area with vertical 18%→0 gradient.
      const grad = ctx.createLinearGradient(0, PAD_T, 0, bot);
      grad.addColorStop(0, `rgba(${rgb.r},${rgb.g},${rgb.b},0.18)`);
      grad.addColorStop(1, `rgba(${rgb.r},${rgb.g},${rgb.b},0)`);
      ctx.fillStyle = grad;
      ctx.beginPath();
      ctx.moveTo(this.#x(seg[0].t), bot);
      for (const s of seg) ctx.lineTo(this.#x(s.t), this.#yL(s.bps));
      ctx.lineTo(this.#x(seg[seg.length - 1].t), bot);
      ctx.closePath();
      ctx.fill();

      // Solid 1.5px stroke.
      ctx.strokeStyle = stroke;
      ctx.lineWidth = 1.5;
      ctx.lineJoin = "round";
      ctx.beginPath();
      seg.forEach((s, i) => {
        const x = this.#x(s.t);
        const y = this.#yL(s.bps);
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      });
      ctx.stroke();
    }
  }

  #drawLatency(ctx: CanvasRenderingContext2D, all: LatencySample[]): void {
    if (all.length < 2) return;
    ctx.lineWidth = 1;
    let prev: LatencySample | null = null;
    for (const s of all) {
      if (s.lost) {
        prev = null;
        continue;
      }
      if (prev) {
        ctx.strokeStyle = s.underLoad || prev.underLoad ? this.#c.warn : this.#c.signal;
        ctx.beginPath();
        ctx.moveTo(this.#x(prev.t), this.#yR(prev.rttMs));
        ctx.lineTo(this.#x(s.t), this.#yR(s.rttMs));
        ctx.stroke();
      }
      prev = s;
    }
  }

  #drawAxesLabels(ctx: CanvasRenderingContext2D): void {
    const top = PAD_T;
    const bot = this.#h - PAD_B;
    ctx.font = "10px var(--font-mono), monospace";
    ctx.fillStyle = this.#c.textSoft;
    // Left: throughput. Right: latency. Top + 50% + (near bottom).
    for (let i = 0; i <= 2; i++) {
      const frac = i / 2; // 0 top, 1 bottom
      const y = top + (bot - top) * frac;
      const bps = this.#vp.bpsMax * (1 - frac);
      const rtt = this.#vp.rttMax * (1 - frac);
      ctx.textAlign = "left";
      ctx.fillText(this.#fmt.throughput(bps), 4, y + 3);
      ctx.textAlign = "right";
      ctx.fillText(this.#fmt.latency(rtt), this.#w - 4, y + 3);
    }
  }

  #drawHover(ctx: CanvasRenderingContext2D, d: ChartData): void {
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
    ctx.fillStyle = this.#c.brand;
    if (info.bps != null) {
      const ty = this.#nearest(d.throughput, info.t);
      if (ty) {
        ctx.beginPath();
        ctx.arc(x, this.#yL(ty.bps), 2.5, 0, Math.PI * 2);
        ctx.fill();
      }
    }
    if (info.rtt != null) {
      ctx.fillStyle = this.#c.warn;
      ctx.beginPath();
      ctx.arc(x, this.#yR(info.rtt), 2.5, 0, Math.PI * 2);
      ctx.fill();
    }
  }
}
