import type { Phase } from "../runner/contract";
import { presentation, type PresentationHandle } from "./presentation";
import { sweepTarget, angleForFraction, interpolateSweep } from "./gaugeSweep";
import { gaugeLayout, type GaugeLayout } from "./gaugeLayout";
import { canvasPixelRatio } from "./canvasResolution";
import {
  resultGaugeFillTarget,
  sortResultGaugeArcs,
  type ResultArcPhase,
} from "../components/resultGauge";
interface GaugeResultArc {
  phase: ResultArcPhase;
  fraction: number;
  dashed: boolean;
}
interface GaugeState {
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
export class GaugeEngine {
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
  #completedSweep = 0;
  #resultColors: Record<ResultArcPhase, string> = {
    download: "#4da3ff",
    upload: "#9b7cff",
    bidirectional: "#2fcca0",
  };
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
    this.#completedSweep = 0;
  }
  invalidateTheme(): void {
    if (!this.#canvas || !this.#ctx) return;
    this.#resolveColors(this.#lastPhase ?? "idle");
    this.wake();
  }
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
    for (const phase of ["download", "upload", "bidirectional"] as const)
      this.#resultColors[phase] = this.#cssVar(
        `--phase-${phase}`,
        this.#resultColors[phase],
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
    this.#resultArcs = sortResultGaugeArcs(
      (s.resultArcs ?? []).map((arc) => ({
        phase: arc.phase,
        label: arc.phase,
        bytesPerSec: arc.fraction,
        dashed: arc.dashed,
      })),
    ).map((arc) => ({
      phase: arc.phase,
      fraction: arc.bytesPerSec,
      dashed: arc.dashed,
    }));
    const enteringComplete =
      s.phase === "complete" && this.#lastPhase !== "complete";
    if (enteringComplete && this.#resultArcs.length) {
      // Start completed-layer animation at zero so the result is visibly earned.
      this.#completedSweep = 0;
    } else if (s.phase !== "complete" || !this.#resultArcs.length) {
      this.#completedSweep = 0;
    }
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
    const completedTarget =
      s.phase === "complete" && this.#resultArcs.length
        ? resultGaugeFillTarget(this.#resultArcs.map((arc) => arc.fraction))
        : 0;
    const completed = interpolateSweep(
      this.#completedSweep,
      completedTarget,
      dt,
      this.#reducedMotion,
    );
    this.#completedSweep = completed.value;
    return next.active || completed.active;
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
    ctx.strokeStyle = this.#track;
    ctx.lineWidth = 1.5;
    ctx.setLineDash([2, 5]);
    ctx.beginPath();
    ctx.arc(cx, cy, r, layout.arcStart, layout.arcStart + layout.arcSweep);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.strokeStyle = this.#tick;
    for (const tick of layout.majorTicks) {
      ctx.globalAlpha = 0.7;
      ctx.beginPath();
      ctx.moveTo(tick.from.x, tick.from.y);
      ctx.lineTo(tick.to.x, tick.to.y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    if (this.#lastPhase === "complete" && this.#resultArcs.length) {
      // Paint results from highest to lowest on one radius; the finish position clips each layer.
      for (const [index, arc] of this.#resultArcs.entries()) {
        const visibleFraction = Math.min(
          Math.max(0, arc.fraction),
          this.#completedSweep,
        );
        if (visibleFraction <= 0.002) continue;
        const end = angleForFraction(
          visibleFraction,
          layout.arcStart,
          layout.arcSweep,
        );
        if (index > 0) {
          // A narrow track-colored under-stroke separates upper-layer handoffs.
          this.#strokeArc(ctx, layout, end, this.#track, arcW + 2, 0.5);
        }
        this.#strokeArc(
          ctx,
          layout,
          end,
          this.#resultColors[arc.phase],
          arcW,
          1,
          arc.dashed ? [arcW * 1.5, arcW] : [],
        );
      }
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
      const headR = arcW * 0.55;
      const ringW = Math.max(1, headR * 0.22);
      ctx.fillStyle = this.#accent;
      ctx.beginPath();
      ctx.arc(hx, hy, headR, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = this.#track;
      ctx.lineWidth = ringW;
      ctx.beginPath();
      ctx.arc(hx, hy, headR + ringW * 0.5, 0, Math.PI * 2);
      ctx.stroke();
    }
  }
  #strokeArc(
    ctx: CanvasRenderingContext2D,
    layout: GaugeLayout,
    end: number,
    color: string,
    width: number,
    alpha: number,
    dash: number[] = [],
  ): void {
    ctx.save();
    ctx.strokeStyle = color;
    ctx.lineWidth = width;
    ctx.globalAlpha = alpha;
    ctx.setLineDash(dash);
    ctx.beginPath();
    ctx.arc(
      layout.center.x,
      layout.center.y,
      layout.radius,
      layout.arcStart,
      end,
    );
    ctx.stroke();
    ctx.restore();
  }
}
