import type { Phase } from "../runner/contract";
import { presentation, type PresentationHandle } from "./presentation";
import { sweepTarget, angleForFraction, interpolateSweep } from "./gaugeSweep";
import { gaugeLayout, type GaugeLayout } from "./gaugeLayout";
import { canvasPixelRatio } from "./canvasResolution";
import {
  resultGaugeFillTarget,
  resultGaugeHeadPlacements,
  type ResultArcPhase,
  type ResultGaugeHeadPlacement,
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
  throughputEvidence: boolean;
  latencyScaleMs: number;
  layout: GaugeLayout;
  rtt: number;
  completedKind: "speed" | "latency";
  /** Highest result first; the component owns result ordering. */
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
  #surface = "#23262b";
  #bandShade: CanvasGradient | null = null;
  #lastPhase: Phase | null = null;
  #sweep = 0;
  #showValue = true;
  #lastFrame = 0;
  #layout: GaugeLayout = gaugeLayout(0, 0);
  #resultArcs: readonly GaugeResultArc[] = [];
  #resultHeadPlacements: readonly ResultGaugeHeadPlacement[] = [];
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
    this.#bandShade = null;
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
    this.#surface = this.#cssVar("--surface-inset", this.#surface);
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
    this.#resultArcs = s.resultArcs ?? [];
    const headGeometry = this.#headGeometry();
    this.#resultHeadPlacements = resultGaugeHeadPlacements(
      this.#resultArcs.map((arc) => arc.fraction),
      {
        baseRadius: this.#layout.radius,
        arcSweep: this.#layout.arcSweep,
        headRadius: headGeometry.radius,
        borderWidth: headGeometry.borderWidth,
      },
    );
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
      throughputEvidence: s.throughputEvidence,
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
    ctx.lineWidth = arcW;
    ctx.beginPath();
    ctx.arc(cx, cy, r, layout.arcStart, layout.arcStart + layout.arcSweep);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.strokeStyle = this.#tick;
    ctx.lineWidth = 1;
    for (const tick of layout.majorTicks) {
      ctx.globalAlpha = 0.7;
      ctx.beginPath();
      ctx.moveTo(tick.from.x, tick.from.y);
      ctx.lineTo(tick.to.x, tick.to.y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
    if (this.#lastPhase === "complete" && this.#resultArcs.length) {
      // Paint results from highest to lowest on the shared arc; lanes stay stable throughout reveal.
      for (const arc of this.#resultArcs) {
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
      // Leaders sit above arcs and below heads so the primary remains visually dominant.
      for (const [index, arc] of this.#resultArcs.entries()) {
        const placement = this.#resultHeadPlacements[index];
        const visibleFraction = Math.min(
          Math.max(0, arc.fraction),
          this.#completedSweep,
        );
        if (
          !placement ||
          placement.lane === 0 ||
          (visibleFraction <= 0.002 && arc.fraction > 0)
        )
          continue;
        const end = angleForFraction(
          visibleFraction,
          layout.arcStart,
          layout.arcSweep,
        );
        const trueX = cx + Math.cos(end) * r;
        const trueY = cy + Math.sin(end) * r;
        const headX = cx + Math.cos(end) * placement.radius;
        const headY = cy + Math.sin(end) * placement.radius;
        ctx.save();
        ctx.strokeStyle = this.#surface;
        ctx.lineWidth = 4;
        ctx.lineCap = "round";
        ctx.setLineDash([]);
        ctx.beginPath();
        ctx.moveTo(trueX, trueY);
        ctx.lineTo(headX, headY);
        ctx.stroke();
        ctx.strokeStyle = this.#resultColors[arc.phase];
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.moveTo(trueX, trueY);
        ctx.lineTo(headX, headY);
        ctx.stroke();
        ctx.restore();
      }
      // Draw secondary heads first and the primary last for deterministic dominance.
      for (let index = this.#resultArcs.length - 1; index >= 0; index -= 1) {
        const arc = this.#resultArcs[index]!;
        const placement = this.#resultHeadPlacements[index];
        const visibleFraction = Math.min(
          Math.max(0, arc.fraction),
          this.#completedSweep,
        );
        if (!placement || (visibleFraction <= 0.002 && arc.fraction > 0))
          continue;
        const end = angleForFraction(
          visibleFraction,
          layout.arcStart,
          layout.arcSweep,
        );
        this.#drawHead(
          cx + Math.cos(end) * placement.radius,
          cy + Math.sin(end) * placement.radius,
          this.#resultColors[arc.phase],
          arc.dashed,
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
      this.#drawHead(hx, hy, this.#accent);
    }
  }
  #headGeometry(): { radius: number; borderWidth: number } {
    const radius = Math.min(7.5, this.#layout.arcWidth * 0.48);
    const closeHeads = this.#resultArcs.some((arc, index, arcs) =>
      arcs
        .slice(index + 1)
        .some(
          (other) =>
            Math.abs(arc.fraction - other.fraction) *
              this.#layout.arcSweep *
              this.#layout.radius <
            2 * radius + 5,
        ),
    );
    // Compact a close cluster before assigning inward lanes, preserving every angle.
    return {
      radius: closeHeads ? Math.min(4, radius) : radius,
      borderWidth: 1.5,
    };
  }
  #drawHead(x: number, y: number, color: string, hollow = false): void {
    const ctx = this.#ctx;
    if (!ctx) return;
    const { radius, borderWidth } = this.#headGeometry();
    ctx.save();
    ctx.fillStyle = hollow ? this.#surface : color;
    ctx.beginPath();
    ctx.arc(x, y, radius, 0, Math.PI * 2);
    ctx.fill();
    ctx.strokeStyle = this.#surface;
    ctx.lineWidth = borderWidth;
    ctx.beginPath();
    ctx.arc(x, y, radius + borderWidth * 0.5, 0, Math.PI * 2);
    ctx.stroke();
    if (hollow) {
      // The track-colored center keeps partial evidence hollow while the phase color identifies it.
      ctx.strokeStyle = color;
      ctx.lineWidth = Math.max(1, borderWidth * 0.55);
      ctx.beginPath();
      ctx.arc(x, y, radius * 0.68, 0, Math.PI * 2);
      ctx.stroke();
    }
    ctx.restore();
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
    if (!this.#bandShade) {
      const { x, y } = layout.center;
      this.#bandShade = ctx.createRadialGradient(
        x,
        y,
        layout.radius - width / 2,
        x,
        y,
        layout.radius + width / 2,
      );
      this.#bandShade.addColorStop(0, "rgba(255, 255, 255, 0.24)");
      this.#bandShade.addColorStop(0.38, "rgba(255, 255, 255, 0.09)");
      this.#bandShade.addColorStop(0.5, "rgba(255, 255, 255, 0.18)");
      this.#bandShade.addColorStop(0.64, "rgba(0, 0, 0, 0.03)");
      this.#bandShade.addColorStop(1, "rgba(0, 0, 0, 0.08)");
    }
    ctx.strokeStyle = this.#bandShade;
    ctx.stroke();
    ctx.restore();
  }
}
