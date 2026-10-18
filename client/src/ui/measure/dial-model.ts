// The dial's pure rules: the ring's geometry, the rate curve, the result arcs and the needle's course.
import type { TransferStage } from "../../engine/run/schedule";
import { STAGE } from "../../view/words";

/** The ring opens downward: clockwise from the bottom left, angles from +x with y down. */
export const SWEEP_DEG = 270;
export const ARC_START = 0.75 * Math.PI;
export const ARC_SWEEP = (SWEEP_DEG * Math.PI) / 180;
const TICKS = 8;
export const DIAL_LABELS = [0, 0.25, 0.5, 0.75, 1] as const;
const LABEL_CLEARANCE = 8;
const NOTE_GAP = 8;
/** The share of the labeled ring's height its centering moves it down. */
const DROP = (1 - Math.SQRT1_2) / 2;
/** Tick ends over the radius: half an arc of 0.13 r, a 3 px gap, then 0.08 r of tick. */
const TICK_REACH = 1.145;
const EPSILON = 1e-6;

type Anchor = "start" | "center" | "end";
interface Point {
  x: number;
  y: number;
}
export interface DialLayout {
  width: number;
  height: number;
  center: Point;
  radius: number;
  arcWidth: number;
  /** A head is flat in its hue and a little wider than its arc. */
  headRadius: number;
  /** The whole ring, which the track and every result arc stroke. */
  track: string;
  ticks: string;
  labels: (Point & { anchorX: Anchor; anchorY: Anchor })[];
  /** Where a note hung under the ring starts: just under the tick ends. */
  noteTop: number;
  /** How wide a hung note may be and stay between the lowest tick labels. */
  noteWidth: number;
  /** The face's shorter side, or the ring's span when the ring is the smaller: every type size reads it. */
  faceMin: number;
}

const round = (value: number) => Math.round(value * 100) / 100;
const anchor = (direction: number): Anchor =>
  direction < -EPSILON ? "end" : direction > EPSILON ? "start" : "center";

/** One geometry for the SVG and the DOM labels, in CSS pixels of the face; `note` is a band hung under the ring. */
export function dialLayout(faceWidth: number, faceHeight: number, note = 0): DialLayout {
  const width = Math.max(1, faceWidth);
  const height = Math.max(1, faceHeight);
  const radius = Math.max(
    36,
    Math.min(
      width * 0.37,
      (width / 2 - 25) / TICK_REACH,
      ((height - 42) / (1 + Math.SQRT1_2) - 11) / TICK_REACH,
      // The labeled ring stays centered and its note still fits under the tick ends.
      note
        ? ((height / 2 - DROP * LABEL_CLEARANCE - NOTE_GAP - note) / (DROP + Math.SQRT1_2) - 3) /
            TICK_REACH
        : Number.POSITIVE_INFINITY,
    ),
  );
  const arcWidth = Math.max(6, radius * 0.13);
  const tickInner = radius + arcWidth / 2 + 3;
  const tickOuter = tickInner + radius * 0.08;
  // The ring with its labels, centered in the face.
  const center = { x: width / 2, y: height / 2 + DROP * (tickOuter + LABEL_CLEARANCE) };
  const at = (angle: number, distance: number) => ({
    x: center.x + Math.cos(angle) * distance,
    y: center.y + Math.sin(angle) * distance,
  });
  const start = at(ARC_START, radius);
  const end = at(ARC_START + ARC_SWEEP, radius);
  let ticks = "";
  for (let k = 0; k <= TICKS; k++) {
    const angle = ARC_START + (k / TICKS) * ARC_SWEEP;
    const from = at(angle, tickInner);
    const to = at(angle, tickOuter);
    ticks += `M${round(from.x)} ${round(from.y)}L${round(to.x)} ${round(to.y)}`;
  }
  return {
    width,
    height,
    center,
    radius,
    arcWidth,
    headRadius: arcWidth / 2 + 2,
    track: `M${round(start.x)} ${round(start.y)}A${round(radius)} ${round(radius)} 0 1 1 ${round(end.x)} ${round(end.y)}`,
    ticks,
    labels: DIAL_LABELS.map((fraction) => {
      const angle = ARC_START + fraction * ARC_SWEEP;
      // The point is the label's nearest edge or corner on the tick's ray.
      return {
        ...at(angle, tickOuter + LABEL_CLEARANCE),
        anchorX: anchor(Math.cos(angle)),
        anchorY: anchor(Math.sin(angle)),
      };
    }),
    noteTop: center.y + Math.SQRT1_2 * tickOuter + NOTE_GAP,
    noteWidth: Math.max(0, 2 * Math.SQRT1_2 * (tickOuter + LABEL_CLEARANCE) - 2 * NOTE_GAP),
    faceMin: Math.min(width, height, radius * 2.4),
  };
}

// One low-end transfer curve for every rate: a hundredth of the scale already moves the needle an eighth of the ring.
const KNOTS = [0, 0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 1] as const;
const SPANS = KNOTS.length - 1;
const scaleOf = (scale: number) => (Number.isFinite(scale) && scale > 0 ? scale : 1);

/** Where a rate sits on the ring, 0 to 1; a value that is not a finite number sits at 0. */
export function gaugeFraction(bytesPerSec: number, scale: number): number {
  const value = bytesPerSec / scaleOf(scale);
  if (!(value > 0)) return 0;
  if (value >= 1) return 1;
  let i = 1;
  while (value > KNOTS[i]!) i++;
  const low = KNOTS[i - 1]!;
  return (i - 1 + (value - low) / (KNOTS[i]! - low)) / SPANS;
}

export function gaugeValue(fraction: number, scale: number): number {
  const f = Number.isFinite(fraction) ? Math.min(1, Math.max(0, fraction)) : 0;
  if (f >= 1) return scaleOf(scale);
  const i = Math.floor(f * SPANS);
  const low = KNOTS[i]!;
  return scaleOf(scale) * (low + (f * SPANS - i) * (KNOTS[i + 1]! - low));
}

/** A finished transfer stage's result on the ring; a one-sided bidirectional result is dashed and named by its surviving direction. */
export interface ResultArc {
  stage: TransferStage;
  direction: TransferStage;
  bytesPerSec: number;
  dashed: boolean;
}

type Rated = { reportedBytesPerSec: number } | null | undefined;

/** Every transfer result, longest first so shorter ones lie over it; a combined bidirectional value needs both lanes. */
export function resultArcs(
  results: {
    download: Rated;
    upload: Rated;
    bidirectional: { down: Rated; up: Rated } | null;
  } | null,
): ResultArc[] {
  if (!results) return [];
  const arcs: ResultArc[] = [];
  for (const stage of ["download", "upload"] as const) {
    const result = results[stage];
    if (result)
      arcs.push({
        stage,
        direction: stage,
        bytesPerSec: result.reportedBytesPerSec,
        dashed: false,
      });
  }
  const down = results.bidirectional?.down?.reportedBytesPerSec;
  const up = results.bidirectional?.up?.reportedBytesPerSec;
  if (down != null && up != null)
    arcs.push({
      stage: "bidirectional",
      direction: "bidirectional",
      bytesPerSec: down + up,
      dashed: false,
    });
  else if (down != null || up != null)
    arcs.push({
      stage: "bidirectional",
      direction: down != null ? "download" : "upload",
      bytesPerSec: (down ?? up)!,
      dashed: true,
    });
  const value = (arc: ResultArc) =>
    Number.isFinite(arc.bytesPerSec) ? arc.bytesPerSec : -Infinity;
  return arcs.sort((a, b) => value(b) - value(a));
}

/** The result the dial reads out, by role and never by size: download, else upload, else bidirectional. */
export const headlineArc = (arcs: readonly ResultArc[]): ResultArc | null =>
  (["download", "upload", "bidirectional"] as const)
    .map((stage) => arcs.find((arc) => arc.stage === stage))
    .find(Boolean) ?? null;

export const arcLabel = (arc: ResultArc) =>
  arc.dashed
    ? `${STAGE.bidirectional.label}, ${STAGE[arc.direction].label.toLowerCase()} only`
    : STAGE[arc.stage].label;

type Bead = "bead" | "stacked" | "split";

/** How each head sits against the higher ones: nearly level shows its trailing half, overlapping gets a halo. */
export function beadKinds(
  fractions: readonly number[],
  radius: number,
  headRadius: number,
): Bead[] {
  const along = (fraction: number) =>
    (Number.isFinite(fraction) ? fraction : 0) * ARC_SWEEP * radius;
  return fractions.map((fraction, index) => {
    let nearest = Number.POSITIVE_INFINITY;
    for (let i = 0; i < index; i++)
      nearest = Math.min(nearest, Math.abs(along(fraction) - along(fractions[i]!)));
    return nearest < headRadius ? "split" : nearest < headRadius * 2 + 2 ? "stacked" : "bead";
  });
}

const SWEEP_MS = 900;
const SWEEP_MIN_MS = 360;

/** A result sweeps the rest of the ring from where the needle stood, in its share of SWEEP_MS, never under the floor. */
export const resultSweepMs = (from: number) => Math.max(SWEEP_MIN_MS, SWEEP_MS * (1 - from));

/** When the sweep's ease-out-cubic front, setting off at `from`, reaches `fraction`: a bead pops then. */
export const reachMs = (fraction: number, from: number, sweepMs: number) =>
  fraction <= from ? 0 : (1 - Math.cbrt(1 - (fraction - from) / (1 - from))) * sweepMs;

export const RISE_MS = 520;
export const DRAIN_MS = 300;
/** A scale change glides the needle and the result arcs alike. */
export const RESCALE_MS = 360;
/** Frames further apart than this leave the needle at rest between them. */
const FRAME_GAP_MS = 50;

/** What the needle is told when what the dial shows changes, not every frame. */
interface NeedleInput {
  visible: boolean;
  /** A run ending in a speed result: the needle waits where it stands for the result to take over. */
  hold: boolean;
  /** The dial is seen and motion is allowed; otherwise every change snaps. */
  motion: boolean;
  /** Changes when the scale under the needle does. */
  course: string;
}

/** The needle's course in degrees: it follows the live value and drains to zero between stages. */
export class Needle {
  #from = 0;
  #at = 0;
  #ms = 0;
  /** Degrees per ms the needle had as its move began; a move sets off at that speed. */
  #speed = 0;
  /** Where a move lands: on the live value it follows, at zero, or where the needle already is. */
  #end: "target" | "zero" | "still" = "zero";
  #drawn = 0;
  #drawnAt = Number.NEGATIVE_INFINITY;
  #velocity = 0;
  #revealed = false;
  #parked = false;
  #course = "";

  /** The angle at `now`, `target` being the live value's angle or null while the dial shows none. */
  angle(now: number, target: number | null): number {
    const end =
      this.#end === "zero" ? 0 : this.#end === "still" ? this.#from : (target ?? this.#drawn);
    const u = this.#ms > 0 ? Math.max(0, (now - this.#at) / this.#ms) : 1;
    // A cubic Hermite from where the move began, at the speed it had, to rest on its end.
    const angle =
      u >= 1
        ? end
        : this.#from * (2 * u ** 3 - 3 * u ** 2 + 1) +
          this.#speed * this.#ms * (u ** 3 - 2 * u ** 2 + u) +
          end * (3 * u ** 2 - 2 * u ** 3);
    if (now > this.#drawnAt) {
      this.#velocity =
        now - this.#drawnAt <= FRAME_GAP_MS ? (angle - this.#drawn) / (now - this.#drawnAt) : 0;
      this.#drawnAt = now;
    }
    this.#drawn = angle;
    return angle;
  }

  /** A move under way; the needle may still follow a moving value after it. */
  moving(now: number): boolean {
    return now - this.#at < this.#ms;
  }

  /** Lit while it shows a value, and while it drains on to zero; a needle a result took over fades where it stood. */
  lit(visible: boolean, motion: boolean): boolean {
    return visible || (motion && this.#drawn > 0.5 && !this.#parked);
  }

  update({ visible, hold, motion, course }: NeedleInput, now: number): void {
    if (!visible) {
      if (this.#revealed && hold) return this.#move(now, 0, "still");
      if (this.#revealed) this.#move(now, motion ? DRAIN_MS : 0, "zero");
      this.#revealed = false;
      return;
    }
    if (this.#parked) {
      this.#parked = false;
      this.#drawn = 0;
      this.#velocity = 0;
    }
    if (!motion) this.#move(now, 0, "target");
    else if (!this.#revealed) this.#move(now, RISE_MS, "target");
    else if (course !== this.#course) this.#move(now, RESCALE_MS, "target");
    this.#revealed = true;
    this.#course = course;
  }

  /** A result takes over: its sweep runs on from the needle's place, which it returns as a fraction of the ring. */
  takeover(motion: boolean, now: number): number {
    const from = motion && this.#revealed ? Math.min(1, this.#drawn / SWEEP_DEG) : 0;
    this.#parked = this.#revealed;
    this.#revealed = false;
    this.#move(now, 0, "still");
    return from;
  }

  #move(now: number, ms: number, end: "target" | "zero" | "still"): void {
    // A needle drawn this frame or the last carries its speed into the move; one long at rest sets off from rest.
    this.#speed = end !== "still" && now - this.#drawnAt <= FRAME_GAP_MS ? this.#velocity : 0;
    this.#from = this.#drawn;
    this.#at = now;
    this.#ms = ms;
    this.#end = end;
  }
}
