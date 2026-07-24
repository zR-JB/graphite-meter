// Decorative radial gauge. State is pulled only when the shared presentation
// scheduler invalidates it; bounded value interpolation is its only animation.

import type { Phase } from "../runner/contract";
import type { CanvasEngine } from "./contract";
import { presentation, type PresentationHandle } from "./presentation";
import { sweepTarget, angleForFraction, interpolateSweep } from "./gaugeSweep";

export interface GaugeState {
  phase: Phase;
  valueBytesPerSec: number;
  scaleBytesPerSec: number;
  latencyScaleMs: number;
  ticks: string[];
  rtt: number;
  completedKind: "speed" | "latency";
}

const PHASE_VAR: Record<Phase, string> = {
  idle: "--text-soft",
  connecting: "--phase-warmup",
  warmup: "--phase-warmup",
  latency: "--phase-latency",
  download: "--phase-download",
  upload: "--phase-upload",
  bidirectional: "--phase-bidirectional",
  complete: "--phase-complete",
  aborted: "--err",
  error: "--err",
};

/* Dial geometry: a 270° arc with the opening centered at the bottom.
   Canvas angles are clockwise from +x (3 o'clock) with y pointing down. */
const ARC_START = Math.PI * 0.75; // 135° → lower-left (7:30)
const ARC_SWEEP = Math.PI * 1.5; // 270° of travel, ending at lower-right
const MAJOR_TICKS = 9;

export class GaugeEngine implements CanvasEngine {
  #get: () => GaugeState;
  #canvas: HTMLCanvasElement | null = null;
  #ctx: CanvasRenderingContext2D | null = null;
  #presentation: PresentationHandle | null = null;
  #motionQuery: MediaQueryList | null = null;
  #reducedMotion = false;

  #dpr = 1;
  #w = 0; // css px
  #h = 0;

  // Resolved theme colors (re-read on theme/resize via invalidateTheme).
  #accent = "#888";
  #track = "#23262b";
  #tick = "#4a5058";
  #label = "#6a717a";

  #lastPhase: Phase | null = null;
  #sweep = 0;
  #lastFrame = 0;
  #ticks: string[] = []; // quarter labels in the active unit

  // Static geometry and the marker are cached at device resolution.
  #base: HTMLCanvasElement | null = null;
  #baseCtx: CanvasRenderingContext2D | null = null;
  #baseSig = "";
  #head: HTMLCanvasElement | null = null;
  #headCtx: CanvasRenderingContext2D | null = null;
  #headSig = "";
  #headHalf = 0; // sprite half-size in css px (placement offset)

  constructor(get: () => GaugeState) {
    this.#get = get;
  }

  attach(canvas: HTMLCanvasElement): void {
    this.#canvas = canvas;
    this.#ctx = canvas.getContext("2d");
    this.#presentation = presentation.register(canvas, this.#render);
    this.#motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    this.#reducedMotion = this.#motionQuery.matches;
    this.#motionQuery.addEventListener("change", this.#onMotionChange);
    this.invalidateTheme(); // size backing store + resolve colors
  }

  wake(): void {
    this.#presentation?.invalidate();
  }

  destroy(): void {
    this.#presentation?.destroy();
    this.#presentation = null;
    this.#motionQuery?.removeEventListener("change", this.#onMotionChange);
    this.#motionQuery = null;
    this.#canvas = null;
    this.#ctx = null;
    this.#base = null;
    this.#baseCtx = null;
    this.#head = null;
    this.#headCtx = null;
  }

  invalidateTheme(): void {
    if (!this.#canvas || !this.#ctx) return;
    this.#dpr = Math.min(window.devicePixelRatio || 1, 2);
    const rect = this.#canvas.getBoundingClientRect();
    this.#w = Math.max(1, rect.width);
    this.#h = Math.max(1, rect.height);
    this.#canvas.width = Math.round(this.#w * this.#dpr);
    this.#canvas.height = Math.round(this.#h * this.#dpr);
    this.#ctx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    this.#resolveColors(this.#lastPhase ?? "idle");
    this.wake();
  }

  #cssVar(name: string, fallback: string): string {
    const v = getComputedStyle(document.documentElement)
      .getPropertyValue(name)
      .trim();
    return v || fallback;
  }

  #resolveColors(phase: Phase): void {
    this.#accent = this.#cssVar(PHASE_VAR[phase], this.#accent);
    this.#track = this.#cssVar("--surface-2", this.#track);
    this.#tick = this.#cssVar("--border-strong", this.#tick);
    this.#label = this.#cssVar("--text-soft", this.#label);
  }

  #render = (now: number): boolean => {
    const active = this.#step(now);
    this.#draw();
    return active;
  };

  #onMotionChange = (event: MediaQueryListEvent): void => {
    this.#reducedMotion = event.matches;
    this.wake();
  };

  #step(now: number): boolean {
    const s = this.#get();
    this.#ticks = s.ticks;

    if (s.phase !== this.#lastPhase) {
      this.#resolveColors(s.phase);
      this.#lastPhase = s.phase;
    }

    const target = sweepTarget({
      phase: s.phase,
      valueBytesPerSec: s.valueBytesPerSec,
      scaleBytesPerSec: s.scaleBytesPerSec,
      latencyScaleMs: s.latencyScaleMs,
      rtt: s.rtt,
      completedKind: s.completedKind,
    });

    const dt = this.#lastFrame ? Math.min(100, now - this.#lastFrame) : 100;
    this.#lastFrame = now;
    const next = interpolateSweep(this.#sweep, target, dt, this.#reducedMotion);
    this.#sweep = next.value;
    return next.active;
  }

  #draw(): void {
    const ctx = this.#ctx;
    if (!ctx) return;
    ctx.clearRect(0, 0, this.#w, this.#h);

    const cx = this.#w / 2;
    const cy = this.#h / 2;
    const m = Math.min(this.#w, this.#h);
    const r = Math.max(36, Math.min(m * 0.37, (m / 2 - 20) / 1.145));
    const arcW = Math.max(6, r * 0.13);
    const sweep = this.#sweep;
    const valueEnd = angleForFraction(sweep, ARC_START, ARC_SWEEP);

    ctx.lineCap = "round";

    this.#ensureBase(cx, cy, r, arcW);
    if (this.#base) {
      ctx.save();
      ctx.setTransform(1, 0, 0, 1, 0, 0); // blit device-px sprite without the dpr scale
      ctx.drawImage(this.#base, 0, 0);
      ctx.restore();
    }

    if (sweep > 0.002) {
      ctx.strokeStyle = this.#accent;
      ctx.lineWidth = arcW;
      ctx.beginPath();
      ctx.arc(cx, cy, r, ARC_START, valueEnd);
      ctx.stroke();

      const hx = cx + Math.cos(valueEnd) * r;
      const hy = cy + Math.sin(valueEnd) * r;
      this.#ensureHead(arcW);
      if (this.#head) {
        const off = this.#headHalf * this.#dpr;
        ctx.save();
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.drawImage(this.#head, hx * this.#dpr - off, hy * this.#dpr - off);
        ctx.restore();
      }
    }
  }

  #ensureBase(cx: number, cy: number, r: number, arcW: number): void {
    const scaleMeaningful =
      this.#lastPhase === "download" ||
      this.#lastPhase === "upload" ||
      this.#lastPhase === "bidirectional" ||
      this.#lastPhase === "complete" ||
      this.#lastPhase === "latency";
    const showLabels = scaleMeaningful && this.#ticks.length >= 2;
    const sig =
      `${this.#w}x${this.#h}@${this.#dpr}|${this.#track}|${this.#tick}|${this.#label}` +
      `|${showLabels ? this.#ticks.join("~") : ""}`;
    if (sig === this.#baseSig && this.#base) return;
    this.#baseSig = sig;

    if (!this.#base) {
      this.#base = document.createElement("canvas");
      this.#baseCtx = this.#base.getContext("2d");
    }
    const sprite = this.#base;
    const ctx = this.#baseCtx;
    if (!ctx) return;
    sprite.width = Math.round(this.#w * this.#dpr);
    sprite.height = Math.round(this.#h * this.#dpr);
    ctx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    ctx.clearRect(0, 0, this.#w, this.#h);
    ctx.lineCap = "round";

    ctx.strokeStyle = this.#track;
    ctx.lineWidth = 1.5;
    ctx.setLineDash([2, 5]);
    ctx.beginPath();
    ctx.arc(cx, cy, r, ARC_START, ARC_START + ARC_SWEEP);
    ctx.stroke();
    ctx.setLineDash([]);

    const hubGap = 2; // px gap from centre before each tick starts
    const hubTick = 4; // px length of each tick
    const hubRing = 6; // px radius of the faint outer ring
    ctx.lineCap = "butt";
    ctx.strokeStyle = this.#tick;
    ctx.lineWidth = 1;
    ctx.globalAlpha = 0.5;
    ctx.beginPath();
    ctx.moveTo(cx, cy - hubGap);
    ctx.lineTo(cx, cy - hubGap - hubTick);
    ctx.moveTo(cx, cy + hubGap);
    ctx.lineTo(cx, cy + hubGap + hubTick);
    ctx.moveTo(cx - hubGap, cy);
    ctx.lineTo(cx - hubGap - hubTick, cy);
    ctx.moveTo(cx + hubGap, cy);
    ctx.lineTo(cx + hubGap + hubTick, cy);
    ctx.stroke();
    ctx.globalAlpha = 0.25;
    ctx.beginPath();
    ctx.arc(cx, cy, hubRing, 0, Math.PI * 2);
    ctx.stroke();
    ctx.globalAlpha = 1;
    ctx.lineCap = "round"; // the major-tick loop below depends on the round cap

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

    if (showLabels) {
      ctx.font = '600 8.5px "JetBrains Mono", monospace';
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
  }

  #ensureHead(arcW: number): void {
    const headR = arcW * 0.55;
    const ringW = Math.max(1, headR * 0.22);
    const half = headR + ringW + 2; // css px; includes ring margin
    this.#headHalf = half;
    const sig = `${this.#accent}|${arcW}|${this.#dpr}`;
    if (sig === this.#headSig && this.#head) return;
    this.#headSig = sig;

    if (!this.#head) {
      this.#head = document.createElement("canvas");
      this.#headCtx = this.#head.getContext("2d");
    }
    const sprite = this.#head;
    const ctx = this.#headCtx;
    if (!ctx) return;
    const side = Math.ceil(2 * half * this.#dpr);
    sprite.width = side;
    sprite.height = side;
    ctx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    ctx.clearRect(0, 0, side / this.#dpr, side / this.#dpr);
    ctx.fillStyle = this.#accent;
    ctx.beginPath();
    ctx.arc(half, half, headR, 0, Math.PI * 2);
    ctx.fill();
    ctx.strokeStyle = this.#track;
    ctx.lineWidth = ringW;
    ctx.beginPath();
    ctx.arc(half, half, headR + ringW * 0.5, 0, Math.PI * 2);
    ctx.stroke();
  }
}
