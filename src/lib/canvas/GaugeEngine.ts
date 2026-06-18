/* ============================================================
 * The Graphite Meter — GaugeEngine (§13.2, supersedes §3.1)
 * The signature visualization: a 270° radial gauge. Industrial,
 * not gamified — a precision instrument dial. Driven by its own
 * rAF loop, pulling a GaugeState snapshot each frame and self-
 * normalizing against a per-phase running peak (so the sweep is
 * smooth and jitter-free regardless of the raw sample rate).
 *
 * The big live number is rendered as plain DOM by <ReactorStage>
 * and sits centered inside this dial; the canvas is decorative
 * (aria-hidden). Visual design is the one area of explicit
 * creative latitude in the port (owner decision).
 * ============================================================ */

import type { Phase } from "../runner/contract";
import type { CanvasEngine } from "./contract";

/** What the engine pulls each frame. The sweep is normalized against an
 *  ABSOLUTE scale (`scaleBps`, shared by download + upload for comparability)
 *  rather than a per-phase peak, so the dial position is meaningful at a
 *  glance. `valueBps` is the current raw throughput. `ticks` are the 5
 *  pre-formatted quarter labels (0 … scale) in the active display unit. */
export interface GaugeState {
  phase: Phase;
  valueBps: number;
  scaleBps: number;
  ticks: string[];
  rtt: number;
  pingCount: number;
}

/** Phase → accent token. Mirrors the reactor's mapping so the dial tints
 *  to the active phase via the same design tokens (no hardcoded color). */
const PHASE_VAR: Record<Phase, string> = {
  idle: "--text-soft",
  warmup: "--phase-warmup",
  latency: "--phase-latency",
  download: "--phase-download",
  upload: "--phase-upload",
  complete: "--phase-complete",
  aborted: "--err",
  error: "--err",
};

const EMA_ALPHA = 0.15; // intensity follower (smooth, jitter-free)
const FILL_ALPHA = 0.1; // slower follower used under reduced-motion
const RIPPLE_MS = 900; // ping ripple lifetime during the latency phase

/* Dial geometry: a 270° arc with the opening centered at the bottom.
   Canvas angles are clockwise from +x (3 o'clock) with y pointing down. */
const ARC_START = Math.PI * 0.75; // 135° → lower-left (7:30)
const ARC_SWEEP = Math.PI * 1.5; // 270° of travel, ending at lower-right
const MAJOR_TICKS = 9;

export class GaugeEngine implements CanvasEngine {
  #get: () => GaugeState;
  #canvas: HTMLCanvasElement | null = null;
  #ctx: CanvasRenderingContext2D | null = null;
  #raf = 0;
  #running = false;

  #dpr = 1;
  #w = 0; // css px
  #h = 0;

  // Resolved theme colors (re-read on theme/resize via invalidateTheme).
  #accent = "#888";
  #track = "#2a2f36";
  #tick = "#454b54";
  #label = "#726d83";

  #lastPhase: Phase | null = null;
  #ema = 0; // smoothed normalized sweep 0–1
  #fill = 0; // slower follower (reduced-motion)
  #rttPeak = 0; // running rtt peak for the latency-phase sweep
  #scale = 1; // absolute throughput scale (bit/s) for normalization
  #ticks: string[] = []; // quarter labels in the active unit
  #frozen = 0; // sweep value held through the complete phase
  #lastPing = 0;
  #ripples: number[] = []; // start timestamps of active ping ripples
  #reduced = false;

  constructor(get: () => GaugeState) {
    this.#get = get;
    this.#reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  }

  attach(canvas: HTMLCanvasElement): void {
    this.#canvas = canvas;
    this.#ctx = canvas.getContext("2d");
    this.invalidateTheme(); // size backing store + resolve colors
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
    this.#ripples = [];
  }

  /** Re-resolve DPR + theme colors; resize the backing store crisply. */
  invalidateTheme(): void {
    if (!this.#canvas || !this.#ctx) return;
    this.#dpr = window.devicePixelRatio || 1;
    const rect = this.#canvas.getBoundingClientRect();
    this.#w = Math.max(1, rect.width);
    this.#h = Math.max(1, rect.height);
    this.#canvas.width = Math.round(this.#w * this.#dpr);
    this.#canvas.height = Math.round(this.#h * this.#dpr);
    this.#ctx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    this.#resolveColors(this.#lastPhase ?? "idle");
  }

  #cssVar(name: string, fallback: string): string {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return v || fallback;
  }

  #resolveColors(phase: Phase): void {
    this.#accent = this.#cssVar(PHASE_VAR[phase], this.#accent);
    this.#track = this.#cssVar("--surface-2", this.#track);
    this.#tick = this.#cssVar("--border-strong", this.#tick);
    this.#label = this.#cssVar("--text-soft", this.#label);
  }

  #loop = (now: number): void => {
    if (!this.#running) return;
    this.#step(now);
    this.#draw(now);
    this.#raf = requestAnimationFrame(this.#loop);
  };

  #step(now: number): void {
    const s = this.#get();

    this.#scale = s.scaleBps > 0 ? s.scaleBps : 1;
    this.#ticks = s.ticks;

    if (s.phase !== this.#lastPhase) {
      // Freeze the dial where it ended when entering `complete`.
      if (s.phase === "complete") this.#frozen = this.#ema;
      this.#resolveColors(s.phase);
      this.#lastPhase = s.phase;
    }

    // Normalize the sweep target to 0–1 against the ABSOLUTE scale so the
    // dial position means the same thing across download + upload (and runs).
    let target = 0;
    if (s.phase === "download" || s.phase === "upload") {
      target = Math.min(1, Math.max(0, s.valueBps / this.#scale));
    } else if (s.phase === "warmup") {
      target = 0.3; // indeterminate — connection probe, no meaningful rate yet
    } else if (s.phase === "latency") {
      // Reads RTT during latency: sweep tracks relative rtt, kept in the
      // lower half of the dial since this phase isn't about throughput.
      this.#rttPeak = Math.max(this.#rttPeak, s.rtt);
      const rel = this.#rttPeak > 0 ? s.rtt / this.#rttPeak : 0;
      target = 0.12 + rel * 0.45;
    } else if (s.phase === "idle") {
      target = 0.1;
    } else if (s.phase === "complete") {
      target = this.#frozen;
    } else {
      target = 0.05; // aborted / error
    }

    this.#ema += EMA_ALPHA * (target - this.#ema);
    this.#fill += FILL_ALPHA * (target - this.#fill);

    // Queue a ripple for each new latency sample since the last frame.
    if (s.pingCount > this.#lastPing) {
      const add = Math.min(3, s.pingCount - this.#lastPing);
      for (let i = 0; i < add; i++) this.#ripples.push(now);
      this.#lastPing = s.pingCount;
    } else if (s.pingCount < this.#lastPing) {
      this.#lastPing = s.pingCount; // run reset
    }
    this.#ripples = this.#ripples.filter((t) => now - t < RIPPLE_MS);
  }

  #draw(now: number): void {
    const ctx = this.#ctx;
    if (!ctx) return;
    ctx.clearRect(0, 0, this.#w, this.#h);

    const cx = this.#w / 2;
    const cy = this.#h / 2;
    const r = Math.max(36, Math.min(this.#w, this.#h) * 0.37);
    const arcW = Math.max(6, r * 0.13);
    const sweep = this.#reduced ? this.#fill : this.#ema;
    const valueEnd = ARC_START + ARC_SWEEP * Math.min(1, Math.max(0, sweep));

    ctx.lineCap = "round";

    // Latency ripples: concentric rings expanding from the hub (skip in
    // reduced-motion — purely decorative).
    if (!this.#reduced) {
      for (const t of this.#ripples) {
        const age = (now - t) / RIPPLE_MS; // 0–1
        ctx.globalAlpha = (1 - age) * 0.35;
        ctx.strokeStyle = this.#accent;
        ctx.lineWidth = 1.25;
        ctx.beginPath();
        ctx.arc(cx, cy, r * (0.28 + age * 0.82), 0, Math.PI * 2);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
    }

    // Background track (the full 270° groove).
    ctx.strokeStyle = this.#track;
    ctx.lineWidth = arcW;
    ctx.beginPath();
    ctx.arc(cx, cy, r, ARC_START, ARC_START + ARC_SWEEP);
    ctx.stroke();

    // Tick marks just outside the groove.
    ctx.strokeStyle = this.#tick;
    ctx.lineWidth = 1.5;
    const tIn = r + arcW * 0.5 + 3;
    const tOut = tIn + r * 0.08;
    for (let i = 0; i < MAJOR_TICKS; i++) {
      const a = ARC_START + (i / (MAJOR_TICKS - 1)) * ARC_SWEEP;
      const ca = Math.cos(a);
      const sa = Math.sin(a);
      ctx.globalAlpha = 0.7;
      ctx.beginPath();
      ctx.moveTo(cx + ca * tIn, cy + sa * tIn);
      ctx.lineTo(cx + ca * tOut, cy + sa * tOut);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;

    // Quarter scale labels (0 … full scale) in the active unit. Deliberately
    // recessive — small, soft, low-alpha — so they inform without competing
    // with the hero number or the sweep. Aligned radially so they read cleanly
    // around the dial. Skipped during phases where the scale has no meaning.
    const scaleMeaningful =
      this.#lastPhase === "download" ||
      this.#lastPhase === "upload" ||
      this.#lastPhase === "complete" ||
      this.#lastPhase === "idle";
    if (scaleMeaningful && this.#ticks.length >= 2) {
      ctx.font = '600 8.5px "IBM Plex Mono", monospace';
      ctx.fillStyle = this.#label;
      ctx.globalAlpha = 0.5;
      const lr = tOut + 7;
      for (let j = 0; j < this.#ticks.length; j++) {
        const a = ARC_START + (j / (this.#ticks.length - 1)) * ARC_SWEEP;
        const ca = Math.cos(a);
        const sa = Math.sin(a);
        ctx.textAlign = ca < -0.25 ? "right" : ca > 0.25 ? "left" : "center";
        ctx.textBaseline = sa < -0.25 ? "bottom" : sa > 0.25 ? "top" : "middle";
        ctx.fillText(this.#ticks[j], cx + ca * lr, cy + sa * lr);
      }
      ctx.globalAlpha = 1;
    }

    // Value arc — the live sweep, phase-tinted, with a soft glow.
    if (sweep > 0.002) {
      if (!this.#reduced) {
        ctx.shadowColor = this.#accent;
        ctx.shadowBlur = 14;
      }
      ctx.strokeStyle = this.#accent;
      ctx.lineWidth = arcW;
      ctx.beginPath();
      ctx.arc(cx, cy, r, ARC_START, valueEnd);
      ctx.stroke();
      ctx.shadowBlur = 0;

      // Glowing head at the leading edge of the sweep.
      const hx = cx + Math.cos(valueEnd) * r;
      const hy = cy + Math.sin(valueEnd) * r;
      ctx.fillStyle = this.#accent;
      if (!this.#reduced) {
        ctx.shadowColor = this.#accent;
        ctx.shadowBlur = 16;
      }
      ctx.beginPath();
      ctx.arc(hx, hy, arcW * 0.62, 0, Math.PI * 2);
      ctx.fill();
      ctx.shadowBlur = 0;
    }
  }
}
