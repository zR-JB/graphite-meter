// Decorative radial gauge. State is pulled only when the shared presentation
// scheduler invalidates it; bounded value interpolation is its only animation.

import type { Phase } from "../runner/contract";
import type { CanvasEngine } from "./contract";
import { presentation, type PresentationHandle } from "./presentation";
import { sweepTarget, angleForFraction, interpolateSweep } from "./gaugeSweep";
import { gaugeLayout, type GaugeLayout } from "./gaugeLayout";
import { canvasPixelRatio } from "./canvasResolution";
import type { ResultArcPhase } from "../components/resultGauge";

export interface GaugeResultArc {
  phase: ResultArcPhase;
  fraction: number;
  dashed: boolean;
}

export interface GaugeState {
  phase: Phase;
  /** Failed stages with no retained result show the dial base without a head. */
  showValue?: boolean;
  valueBytesPerSec: number;
  scaleBytesPerSec: number;
  latencyScaleMs: number;
  layout: GaugeLayout;
  rtt: number;
  completedKind: "speed" | "latency";
  resultArcs?: readonly GaugeResultArc[];
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

  // Resolved theme colors (re-read on theme invalidation only).
  #accent = "#888";
  #track = "#23262b";
  #tick = "#4a5058";

  #lastPhase: Phase | null = null;
  #sweep = 0;
  #showValue = true;
  #lastFrame = 0;
  #layout: GaugeLayout = gaugeLayout(0, 0, 0);
  #resultArcs: readonly GaugeResultArc[] = [];
  #resultColors: Record<ResultArcPhase, string> = {
    download: "#4da3ff",
    upload: "#9b7cff",
    bidirectional: "#2fcca0",
  };

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
    this.#presentation = presentation.register(canvas, this.#render, {
      nativeAnimation: true,
    });
    this.#motionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    this.#reducedMotion = this.#motionQuery.matches;
    this.#motionQuery.addEventListener("change", this.#onMotionChange);
    this.invalidateTheme();
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
    this.#resolveColors(this.#lastPhase ?? "idle");
    this.#baseSig = "";
    this.#headSig = "";
    this.wake();
  }

  /**
   * The component that owns CSS layout supplies the exact same dimensions used
   * for its DOM labels. Zero-sized observations are transient (for example,
   * hidden tabs) and deliberately retain the last valid backing store.
   */
  resize(cssWidth: number, cssHeight: number): void {
    if (!this.#canvas || !this.#ctx) return;
    if (
      !Number.isFinite(cssWidth) ||
      !Number.isFinite(cssHeight) ||
      cssWidth <= 0 ||
      cssHeight <= 0
    )
      return;

    const dpr = canvasPixelRatio();
    const backingWidth = Math.max(1, Math.round(cssWidth * dpr));
    const backingHeight = Math.max(1, Math.round(cssHeight * dpr));
    if (
      this.#w === cssWidth &&
      this.#h === cssHeight &&
      this.#dpr === dpr &&
      this.#canvas.width === backingWidth &&
      this.#canvas.height === backingHeight
    )
      return;

    this.#w = cssWidth;
    this.#h = cssHeight;
    this.#dpr = dpr;
    this.#canvas.width = backingWidth;
    this.#canvas.height = backingHeight;
    this.#ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    // Every cached sprite is device-resolution geometry. They must be rebuilt
    // against this backing store as one resize generation.
    this.#baseSig = "";
    this.#headSig = "";
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
    this.#resultColors.download = this.#cssVar(
      "--phase-download",
      this.#resultColors.download,
    );
    this.#resultColors.upload = this.#cssVar(
      "--phase-upload",
      this.#resultColors.upload,
    );
    this.#resultColors.bidirectional = this.#cssVar(
      "--phase-bidirectional",
      this.#resultColors.bidirectional,
    );
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
    this.#layout = s.layout;
    this.#showValue = s.showValue ?? true;
    this.#resultArcs = s.resultArcs ?? [];

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
    if (!ctx || this.#w <= 0 || this.#h <= 0) return;
    ctx.clearRect(0, 0, this.#w, this.#h);

    const layout = this.#layout;
    const { x: cx, y: cy } = layout.center;
    const r = layout.radius;
    const arcW = layout.arcWidth;
    const sweep = this.#sweep;
    const valueEnd = angleForFraction(sweep, layout.arcStart, layout.arcSweep);

    ctx.lineCap = "round";

    this.#ensureBase(layout);
    if (this.#base) {
      ctx.save();
      ctx.setTransform(1, 0, 0, 1, 0, 0); // blit device-px sprite without the dpr scale
      ctx.drawImage(this.#base, 0, 0);
      ctx.restore();
    }

    if (this.#lastPhase === "complete" && this.#resultArcs.length) {
      const count = this.#resultArcs.length;
      const lineWidth = count === 1 ? arcW : Math.max(3, arcW * 0.36);
      const spacing = lineWidth + 2;
      this.#resultArcs.forEach((arc, index) => {
        const radius = r + (index - (count - 1) / 2) * spacing;
        ctx.strokeStyle = this.#resultColors[arc.phase];
        ctx.lineWidth = lineWidth;
        ctx.setLineDash(arc.dashed ? [lineWidth * 1.5, lineWidth] : []);
        ctx.beginPath();
        ctx.arc(
          cx,
          cy,
          radius,
          layout.arcStart,
          angleForFraction(arc.fraction, layout.arcStart, layout.arcSweep),
        );
        ctx.stroke();
      });
      ctx.setLineDash([]);
      return;
    }

    if (this.#showValue && sweep > 0.002) {
      ctx.strokeStyle = this.#accent;
      ctx.lineWidth = arcW;
      ctx.beginPath();
      ctx.arc(cx, cy, r, layout.arcStart, valueEnd);
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

  #ensureBase(layout: GaugeLayout): void {
    const { x: cx, y: cy } = layout.center;
    const r = layout.radius;
    const sig = `${this.#geometrySignature(layout)}@${this.#dpr}|${this.#track}|${this.#tick}`;
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
    ctx.arc(cx, cy, r, layout.arcStart, layout.arcStart + layout.arcSweep);
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
    for (const tick of layout.majorTicks) {
      ctx.globalAlpha = 0.7;
      ctx.beginPath();
      ctx.moveTo(tick.from.x, tick.from.y);
      ctx.lineTo(tick.to.x, tick.to.y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
  }

  #geometrySignature(layout: GaugeLayout): string {
    return [
      layout.width,
      layout.height,
      layout.center.x,
      layout.center.y,
      layout.radius,
      layout.arcWidth,
      layout.arcStart,
      layout.arcSweep,
      ...layout.majorTicks.flatMap((tick) => [
        tick.from.x,
        tick.from.y,
        tick.to.x,
        tick.to.y,
      ]),
    ].join(",");
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
