/* ============================================================
 * The Graphite Meter — GaugeEngine
 * The signature visualization: a 270° radial gauge. Industrial,
 * not gamified — a precision instrument dial. Driven by its own
 * rAF loop, pulling a GaugeState snapshot each frame and self-
 * normalizing against a per-phase running peak (so the sweep is
 * smooth and jitter-free regardless of the raw sample rate).
 *
 * The big live number is rendered as plain DOM by <GaugePanel>
 * and sits centered inside this dial; the canvas is decorative
 * (aria-hidden). Visual design is the one area of explicit
 * creative latitude in the port (owner decision).
 * ============================================================ */

import type { Phase } from "../runner/contract";
import type { CanvasEngine } from "./contract";
import { sweepTarget, angleForFraction } from "./gaugeSweep";

/** What the engine pulls each frame. The sweep is normalized against an
 *  ABSOLUTE scale (`scaleBytesPerSec`, shared by download + upload for comparability)
 *  rather than a per-phase peak, so the dial position is meaningful at a
 *  glance. `valueBytesPerSec` is the current raw throughput. `latencyScaleMs`
 *  is the linear ms full-scale used during the latency phase. `ticks` are the 5
 *  pre-formatted quarter labels (0 … scale) — throughput in the active display
 *  unit, or ms during the latency phase. */
export interface GaugeState {
  phase: Phase;
  valueBytesPerSec: number;
  scaleBytesPerSec: number;
  latencyScaleMs: number;
  ticks: string[];
  rtt: number;
  pingCount: number;
  /** At `complete`, the 0–1 dial position to rest on for the primary result
   *  stage (download→upload→latency, picked by the store). -1 when not
   *  resolved — the dial then holds wherever the last live phase left it. */
  resolvedFraction: number;
}

/** Phase → accent token. Mirrors the gauge panel's mapping so the dial tints
 *  to the active phase via the same design tokens (no hardcoded color). */
const PHASE_VAR: Record<Phase, string> = {
  idle: "--text-soft",
  warmup: "--phase-warmup",
  latency: "--phase-latency",
  download: "--phase-download",
  upload: "--phase-upload",
  bidirectional: "--phase-bidirectional",
  complete: "--phase-complete",
  aborted: "--err",
  error: "--err",
};

const EMA_ALPHA = 0.15; // intensity follower (smooth, jitter-free)
const FILL_ALPHA = 0.1; // slower follower used under reduced-motion
const RIPPLE_MS = 900; // ping ripple lifetime during the latency phase
/** Minimum gap between ripple emissions. The ping stream reports many samples a
 *  second (≈50/s on a fast link), but one ring per sample stacks into a frantic,
 *  cheap-looking flash. Gate emission to a calm, deliberate pulse independent of
 *  the sample rate — the rings still animate over RIPPLE_MS, just fewer at once. */
const RIPPLE_MIN_GAP_MS = 240;

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
  #track = "#23262b";
  #tick = "#4a5058";
  #label = "#6a717a";

  #lastPhase: Phase | null = null;
  #lastNow = 0; // timestamp of the last drawn frame (reused for resize repaints)
  #target = 0; // last computed sweep target (drives self-park decision)
  #ema = 0; // smoothed normalized sweep 0–1
  #fill = 0; // slower follower (reduced-motion)
  #scale = 1; // absolute throughput scale (bytes/s) for normalization
  #ticks: string[] = []; // quarter labels in the active unit
  #frozen = 0; // sweep value held through the complete phase
  #lastPing = 0;
  #lastRippleAt = 0; // timestamp of the last emitted ripple (throttle gate)
  #ripples: number[] = []; // start timestamps of active ping ripples
  #reduced = false;

  // ── Cached layers (pure performance; pixel-identical to drawing inline) ──
  // Static base = track + ticks + scale labels. These use only theme-fixed
  // colors + geometry, so they're rendered once to an offscreen canvas and
  // blitted each frame; rebuilt only when the signature below changes.
  #base: HTMLCanvasElement | null = null;
  #baseCtx: CanvasRenderingContext2D | null = null;
  #baseSig = "";
  // The glowing head is the one phase-tinted, blur-heavy sprite. Pre-rendered
  // (circle + shadowBlur) per (accent, size) so we never run shadowBlur on it
  // per frame — only blit. The sweep arc itself stays drawn live.
  #head: HTMLCanvasElement | null = null;
  #headCtx: CanvasRenderingContext2D | null = null;
  #headSig = "";
  #headHalf = 0; // sprite half-size in css px (placement offset)

  constructor(get: () => GaugeState) {
    this.#get = get;
    this.#reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
  }

  attach(canvas: HTMLCanvasElement): void {
    this.#canvas = canvas;
    this.#ctx = canvas.getContext("2d");
    this.invalidateTheme(); // size backing store + resolve colors
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
    this.#ripples = [];
    this.#base = null;
    this.#baseCtx = null;
    this.#head = null;
    this.#headCtx = null;
  }

  /** Re-resolve DPR + theme colors; resize the backing store crisply. DPR is
   *  capped at 2: beyond that the extra raster pixels are indistinguishable on
   *  a dial but quadruple the per-frame fill cost on an iGPU. */
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
    // Resizing the backing store wipes it to transparent. Repaint NOW (the
    // ResizeObserver fires between layout and paint) so the browser never
    // shows a blank canvas mid-resize — otherwise the dial flickers/vanishes
    // while dragging because the redraw waited for the next frame.
    this.#draw(this.#lastNow);
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

  #loop = (now: number): void => {
    if (!this.#running) return;
    this.#lastNow = now;
    this.#step(now);
    this.#draw(now);
    if (this.#animating()) {
      this.#raf = requestAnimationFrame(this.#loop);
    } else {
      // Nothing left to move — park the loop so the GPU can idle. A wake()
      // (state change, theme/resize, hover) re-arms it.
      this.#running = false;
      this.#raf = 0;
    }
  };

  /** True while the dial still has motion in flight. Live phases always
   *  animate (smooth sweep + ripples); idle/complete/error animate only until
   *  the followers settle on their target, then the loop parks. */
  #animating(): boolean {
    const p = this.#lastPhase;
    if (
      p === "warmup" ||
      p === "latency" ||
      p === "download" ||
      p === "upload" ||
      p === "bidirectional"
    )
      return true;
    if (!this.#reduced && this.#ripples.length) return true;
    const sweep = this.#reduced ? this.#fill : this.#ema;
    return Math.abs(this.#target - sweep) > 0.0005;
  }

  #step(now: number): void {
    const s = this.#get();

    this.#scale = s.scaleBytesPerSec > 0 ? s.scaleBytesPerSec : 1;
    this.#ticks = s.ticks;

    if (s.phase !== this.#lastPhase) {
      // Freeze the dial where it ended when entering `complete`.
      if (s.phase === "complete") this.#frozen = this.#ema;
      this.#resolveColors(s.phase);
      this.#lastPhase = s.phase;
    }

    // Normalize the sweep target to 0–1 against the ABSOLUTE scale so the
    // dial position means the same thing across download + upload + the
    // combined bidirectional rate (and runs). The store already sums the
    // live down+up samples into valueBytesPerSec for the bidirectional
    // phase (see liveTransferBytesPerSec), so this is the same formula.
    const target = sweepTarget({
      phase: s.phase,
      valueBytesPerSec: s.valueBytesPerSec,
      scaleBytesPerSec: this.#scale,
      latencyScaleMs: s.latencyScaleMs,
      rtt: s.rtt,
      resolvedFraction: s.resolvedFraction,
      frozenFraction: this.#frozen,
    });

    this.#target = target;
    this.#ema += EMA_ALPHA * (target - this.#ema);
    this.#fill += FILL_ALPHA * (target - this.#fill);

    // Emit at most one ripple per RIPPLE_MIN_GAP_MS, no matter how many samples
    // arrived since the last frame — a deliberate pulse, not one ring per tick.
    if (s.pingCount > this.#lastPing) {
      this.#lastPing = s.pingCount;
      if (now - this.#lastRippleAt >= RIPPLE_MIN_GAP_MS) {
        this.#ripples.push(now);
        this.#lastRippleAt = now;
      }
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
    // Radius: 0.37 of the smaller dimension, capped so the dial's FULL extent
    // fits the canvas — beyond r there is half the arc width (0.065r), the
    // tick (3px gap + 0.08r), the label gap (7px) and the label text (~10px);
    // without the cap the top tick label clips on short containers.
    const m = Math.min(this.#w, this.#h);
    const r = Math.max(36, Math.min(m * 0.37, (m / 2 - 20) / 1.145));
    const arcW = Math.max(6, r * 0.13);
    const sweep = this.#reduced ? this.#fill : this.#ema;
    const valueEnd = angleForFraction(sweep, ARC_START, ARC_SWEEP);

    ctx.lineCap = "round";

    // Latency ripples: concentric rings expanding from the hub (skip in
    // reduced-motion — purely decorative). Drawn first so the track groove
    // overdraws them.
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

    // Static base (track + ticks + scale labels) — pre-rendered, blitted 1:1.
    this.#ensureBase(cx, cy, r, arcW);
    if (this.#base) {
      ctx.save();
      ctx.setTransform(1, 0, 0, 1, 0, 0); // blit device-px sprite without the dpr scale
      ctx.drawImage(this.#base, 0, 0);
      ctx.restore();
    }

    // Value arc — the live sweep, phase-tinted. Flat and crisp, like a ruled
    // ink line traced over the pencil construction arc beneath it — no glow.
    if (sweep > 0.002) {
      ctx.strokeStyle = this.#accent;
      ctx.lineWidth = arcW;
      ctx.beginPath();
      ctx.arc(cx, cy, r, ARC_START, valueEnd);
      ctx.stroke();

      // Marker at the leading edge — blitted from the cached sprite (a flat
      // pin, not a glow) so it's computed once per (accent, size).
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

  /** (Re)render the static base layer (track + ticks + scale labels) when its
   *  signature changes. Keyed by geometry + the theme-fixed colors + the label
   *  text/visibility — NOT the phase accent, which never touches these. */
  #ensureBase(cx: number, cy: number, r: number, arcW: number): void {
    // Labels show where the dial position maps to a real value: throughput
    // during download/upload/complete, RTT (ms) during latency. The idle dial
    // and the warmup probe carry no scale, so they stay clean/unlabeled.
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
    const c = this.#base;
    const bx = this.#baseCtx;
    if (!bx) return;
    c.width = Math.round(this.#w * this.#dpr);
    c.height = Math.round(this.#h * this.#dpr);
    bx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    bx.clearRect(0, 0, this.#w, this.#h);
    bx.lineCap = "round";

    // Background track — a pencil construction arc, not a lit metal groove:
    // a thin dashed line at the dial's true radius (the value arc, drawn
    // live and full-width, reads as the ink traced over it).
    bx.strokeStyle = this.#track;
    bx.lineWidth = 1.5;
    bx.setLineDash([2, 5]);
    bx.beginPath();
    bx.arc(cx, cy, r, ARC_START, ARC_START + ARC_SWEEP);
    bx.stroke();
    bx.setLineDash([]);

    // Registration-mark hub at dial centre (thin crosshair + faint ring).
    // Butt caps keep the hairlines crisp; restored to round below.
    const hubGap = 2; // px gap from centre before each tick starts
    const hubTick = 4; // px length of each tick
    const hubRing = 6; // px radius of the faint outer ring
    bx.lineCap = "butt";
    bx.strokeStyle = this.#tick;
    bx.lineWidth = 1;
    bx.globalAlpha = 0.5;
    bx.beginPath();
    bx.moveTo(cx, cy - hubGap);
    bx.lineTo(cx, cy - hubGap - hubTick);
    bx.moveTo(cx, cy + hubGap);
    bx.lineTo(cx, cy + hubGap + hubTick);
    bx.moveTo(cx - hubGap, cy);
    bx.lineTo(cx - hubGap - hubTick, cy);
    bx.moveTo(cx + hubGap, cy);
    bx.lineTo(cx + hubGap + hubTick, cy);
    bx.stroke();
    bx.globalAlpha = 0.25;
    bx.beginPath();
    bx.arc(cx, cy, hubRing, 0, Math.PI * 2);
    bx.stroke();
    bx.globalAlpha = 1;
    bx.lineCap = "round"; // the major-tick loop below depends on the round cap

    // Tick marks just outside the groove.
    bx.strokeStyle = this.#tick;
    bx.lineWidth = 1.5;
    const tIn = r + arcW * 0.5 + 3;
    const tOut = tIn + r * 0.08;
    for (let i = 0; i < MAJOR_TICKS; i++) {
      const a = ARC_START + (i / (MAJOR_TICKS - 1)) * ARC_SWEEP;
      const ca = Math.cos(a);
      const sa = Math.sin(a);
      bx.globalAlpha = 0.7;
      bx.beginPath();
      bx.moveTo(cx + ca * tIn, cy + sa * tIn);
      bx.lineTo(cx + ca * tOut, cy + sa * tOut);
      bx.stroke();
    }
    bx.globalAlpha = 1;

    // Quarter scale labels (0 … full scale) — recessive, radially aligned.
    // Skipped during phases where the scale has no meaning.
    if (showLabels) {
      bx.font = '600 8.5px "JetBrains Mono", monospace';
      bx.fillStyle = this.#label;
      bx.globalAlpha = 0.5;
      const lr = tOut + 7;
      for (let j = 0; j < this.#ticks.length; j++) {
        const a = ARC_START + (j / (this.#ticks.length - 1)) * ARC_SWEEP;
        const ca = Math.cos(a);
        const sa = Math.sin(a);
        bx.textAlign = ca < -0.25 ? "right" : ca > 0.25 ? "left" : "center";
        bx.textBaseline = sa < -0.25 ? "bottom" : sa > 0.25 ? "top" : "middle";
        bx.fillText(this.#ticks[j], cx + ca * lr, cy + sa * lr);
      }
      bx.globalAlpha = 1;
    }
  }

  /** (Re)render the leading-edge marker sprite when accent/size/dpr change. A
   *  flat filled pin with a thin panel-toned ring — like a drafting-compass
   *  pencil point — never a blurred glow. */
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
    const c = this.#head;
    const hx = this.#headCtx;
    if (!hx) return;
    const side = Math.ceil(2 * half * this.#dpr);
    c.width = side;
    c.height = side;
    hx.setTransform(this.#dpr, 0, 0, this.#dpr, 0, 0);
    hx.clearRect(0, 0, side / this.#dpr, side / this.#dpr);
    hx.fillStyle = this.#accent;
    hx.beginPath();
    hx.arc(half, half, headR, 0, Math.PI * 2);
    hx.fill();
    hx.strokeStyle = this.#track;
    hx.lineWidth = ringW;
    hx.beginPath();
    hx.arc(half, half, headR + ringW * 0.5, 0, Math.PI * 2);
    hx.stroke();
  }
}
