import type { RunResult } from "../runner/contract";
import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";
import { STAGE } from "../presentation/vocabulary";

export type ResultArcPhase = "download" | "upload" | "bidirectional";

export interface ResultGaugeArc {
  phase: ResultArcPhase;
  /** The measured direction; a one-sided bidirectional arc names its survivor. */
  direction: ResultArcPhase;
  label: string;
  bytesPerSec: number;
  dashed: boolean;
}

/** How a result's head sits on the ring: every head stays at its own value. */
export type ResultGaugeHead = "bead" | "stacked" | "split";

/** Heads paint from the highest result down, so a lower one lies over its neighbour: one that overlaps a head
    already painted is cut out of it by a halo in the ground ("stacked"), and one almost level with it shows only
    its trailing half, so the pair reads as one bead in both hues ("split"). */
export function resultGaugeHeads(
  fractions: readonly number[],
  geometry: { radius: number; arcSweep: number; headRadius: number },
): ResultGaugeHead[] {
  const along = (fraction: number) =>
    (Number.isFinite(fraction) ? fraction : 0) *
    geometry.arcSweep *
    geometry.radius;
  return fractions.map((fraction, index) => {
    const nearest = Math.min(
      ...fractions
        .slice(0, index)
        .map((other) => Math.abs(along(fraction) - along(other))),
    );
    if (nearest < geometry.headRadius) return "split";
    if (nearest < geometry.headRadius * 2 + 2) return "stacked";
    return "bead";
  });
}

const arcValue = (value: number): number =>
  Number.isFinite(value) ? value : -Infinity;

/** Highest throughput is painted first so lower values can layer over it. */
export function sortResultGaugeArcs(
  arcs: readonly ResultGaugeArc[],
): ResultGaugeArc[] {
  return arcs.toSorted(
    (a, b) => arcValue(b.bytesPerSec) - arcValue(a.bytesPerSec),
  );
}

/** Keep the headline stable by measurement role, independently of paint order. */
export function primaryResultGaugeArc(
  arcs: readonly ResultGaugeArc[],
): ResultGaugeArc | null {
  return (
    arcs.find((arc) => arc.phase === "download") ??
    arcs.find((arc) => arc.phase === "upload") ??
    arcs.find((arc) => arc.phase === "bidirectional") ??
    null
  );
}

export function resultGaugeArcs(result: RunResult | null): ResultGaugeArc[] {
  if (!result) return [];
  const arcs: ResultGaugeArc[] = [];
  const add = (
    phase: ResultArcPhase,
    bytesPerSec: number,
    direction: ResultArcPhase = phase,
  ) =>
    arcs.push({
      phase,
      direction,
      label:
        phase === direction
          ? STAGE[phase].label
          : `${STAGE[phase].label} ${direction}`,
      bytesPerSec,
      dashed: phase !== direction,
    });
  for (const phase of ["download", "upload"] as const) {
    const value = result[phase];
    if (value) add(phase, value.reportedBytesPerSec);
  }
  const bidi = bidirectionalResultPresentation(
    result.bidirectional?.down?.reportedBytesPerSec,
    result.bidirectional?.up?.reportedBytesPerSec,
  );
  if (bidi.combinedBytesPerSec != null) {
    add("bidirectional", bidi.combinedBytesPerSec);
  } else if (bidi.survivingDirection) {
    const value = bidi[bidi.survivingDirection];
    if (value != null) {
      const direction =
        bidi.survivingDirection === "down" ? "download" : "upload";
      add("bidirectional", value, direction);
    }
  }
  return sortResultGaugeArcs(arcs);
}
