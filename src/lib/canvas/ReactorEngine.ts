/* ============================================================
 * The Graphite Meter — ReactorEngine (§3.1)
 * Vertical particle-flow column. Industrial, not gamified: a
 * stylized conduit of data. Driven by its own rAF loop, pulling
 * a ReactorState snapshot each frame. Particle math is the one
 * area of explicit creative latitude in the blueprint.
 * ============================================================ */

import type { Phase } from "../runner/contract";
import type { CanvasEngine } from "./contract";

/** What the engine pulls each frame. `intensity` is the raw display
 *  value (e.g. Mbps); the engine self-normalizes against a running peak. */
export interface ReactorState {
  phase: Phase;
  intensity: number;
  rtt: number;
  pingCount: number;
}

interface Particle {
  x: number;
  y: number;
  r: number; // radius / stroke weight
  a: number; // base alpha
}

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

const COUNT = 110; // pooled, within the 80–140 spec band
const EMA_ALPHA = 0.15; // intensity follower (smooth, jitter-free)
const PULSE_MS = 650; // ping pulse lifetime

export class ReactorEngine implements CanvasEngine {
  #get: () => ReactorState;
  #canvas: HTMLCanvasElement | null = null;
  #ctx: CanvasRenderingContext2D | null = null;
  #raf = 0;
  #running = false;

  #dpr = 1;
  #w = 0; // css px
  #h = 0;

  #particles: Particle[] = [];
  #accent = "#888";
  #lastPhase: Phase | null = null;
  #ema = 0; // smoothed normalized intensity 0–1
  #fill = 0; // smoothed level for reduced-motion bar
  #peak = 0; // per-phase running peak for normalization
  #lastPing = 0;
  #pulses: number[] = []; // start timestamps of active ping pulses
  #last = 0;
  #reduced = false;

  constructor(get: () => ReactorState) {
    this.#get = get;
    this.#reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  }

  attach(canvas: HTMLCanvasElement): void {
    this.#canvas = canvas;
    this.#ctx = canvas.getContext("2d");
    this.invalidateTheme(); // size backing store + resolve accent
    this.#seed();
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#last = performance.now();
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
    this.#particles = [];
    this.#pulses = [];
  }

  /** Re-resolve DPR + theme accent; resize the backing store crisply. */
  invalidateTheme(): void {
    if (!this.#canvas || !this.#ctx) return;
    this.#dpr = window.devicePixelRatio || 1;
    const rect = this.#canvas.getBoundingClientRect();
    this.#w = Math.max(1, rect.width);
    this.#h = Math.max(1, rect.height);
    this.#canvas.width = Math.round(this.#w * this.#dpr);
    this.#canvas.height = Math.round(this.#h * this.#dpr);
    this.#ctx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    this.#resolveAccent(this.#lastPhase ?? "idle");
  }

  #resolveAccent(phase: Phase): void {
    const v = getComputedStyle(document.documentElement)
      .getPropertyValue(PHASE_VAR[phase])
      .trim();
    if (v) this.#accent = v;
  }

  #seed(): void {
    this.#particles = [];
    for (let i = 0; i < COUNT; i++) {
      this.#particles.push({
        x: Math.random() * this.#w,
        y: Math.random() * this.#h,
        r: 0.6 + Math.random() * 1.7,
        a: 0.28 + Math.random() * 0.6,
      });
    }
  }

  #baseV(phase: Phase): number {
    switch (phase) {
      case "download":
      case "upload":
        return 240;
      case "warmup":
        return 140;
      case "complete":
        return 32;
      case "latency":
        return 16;
      case "idle":
        return 24;
      default:
        return 60;
    }
  }

  #loop = (now: number): void => {
    if (!this.#running) return;
    const dt = Math.min(64, now - this.#last) / 1000; // seconds, clamped
    this.#last = now;
    this.#step(dt, now);
    this.#draw(now);
    this.#raf = requestAnimationFrame(this.#loop);
  };

  #step(dt: number, now: number): void {
    const s = this.#get();

    if (s.phase !== this.#lastPhase) {
      this.#peak = 0;
      this.#resolveAccent(s.phase);
      this.#lastPhase = s.phase;
    }

    // Normalize intensity to 0–1 against a per-phase running peak.
    let target = 0;
    if (s.phase === "download" || s.phase === "upload" || s.phase === "warmup") {
      this.#peak = Math.max(this.#peak, s.intensity);
      target = this.#peak > 0 ? s.intensity / this.#peak : 0;
      if (s.phase === "warmup") target = Math.max(target, 0.35);
    } else if (s.phase === "latency") {
      target = 0.08;
    } else if (s.phase === "idle") {
      target = 0.12;
    } else if (s.phase === "complete") {
      target = 0.5;
    }
    this.#ema += EMA_ALPHA * (target - this.#ema);
    this.#fill += 0.12 * (target - this.#fill);

    // Queue ping pulses for any new latency samples since last frame.
    if (s.pingCount > this.#lastPing) {
      const add = Math.min(4, s.pingCount - this.#lastPing);
      for (let i = 0; i < add; i++) this.#pulses.push(now);
      this.#lastPing = s.pingCount;
    }
    this.#pulses = this.#pulses.filter((t) => now - t < PULSE_MS);

    if (this.#reduced) return; // static fallback: no particle motion

    const dir = s.phase === "upload" ? -1 : 1;
    const speed = this.#baseV(s.phase) * (0.3 + this.#ema * 0.7);
    for (const p of this.#particles) {
      p.y += dir * speed * dt * (0.7 + p.r * 0.18);
      if (dir > 0 && p.y > this.#h + 8) {
        p.y = -8;
        p.x = Math.random() * this.#w;
      } else if (dir < 0 && p.y < -8) {
        p.y = this.#h + 8;
        p.x = Math.random() * this.#w;
      }
    }
  }

  #draw(now: number): void {
    const ctx = this.#ctx;
    if (!ctx) return;
    ctx.clearRect(0, 0, this.#w, this.#h);
    const accent = this.#accent;

    if (this.#reduced) {
      // Reduced motion: a single smooth vertical fill bar (height = intensity).
      const barW = Math.min(120, this.#w * 0.4);
      const x = (this.#w - barW) / 2;
      const hgt = this.#h * this.#fill;
      ctx.globalAlpha = 0.9;
      ctx.fillStyle = accent;
      ctx.fillRect(x, this.#h - hgt, barW, hgt);
      ctx.globalAlpha = 1;
    } else {
      const dir = this.#get().phase === "upload" ? -1 : 1;
      ctx.lineCap = "round";
      ctx.strokeStyle = accent;
      const len = 2 + this.#ema * 11;
      for (const p of this.#particles) {
        ctx.globalAlpha = p.a * (0.35 + this.#ema * 0.65);
        ctx.lineWidth = p.r;
        ctx.beginPath();
        ctx.moveTo(p.x, p.y);
        ctx.lineTo(p.x, p.y - dir * len);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
    }

    // Ping pulses: horizontal dashes shooting across (both render modes).
    const y = this.#h * 0.5;
    ctx.strokeStyle = accent;
    ctx.lineWidth = 1.5;
    ctx.lineCap = "round";
    for (const t of this.#pulses) {
      const age = (now - t) / PULSE_MS; // 0–1
      const x = age * this.#w;
      ctx.globalAlpha = (1 - age) * 0.5;
      ctx.beginPath();
      ctx.moveTo(x - 22, y);
      ctx.lineTo(x, y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
  }
}
