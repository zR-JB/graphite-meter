import type { RunResult } from "../runner/contract";
import { bidirectionalResultPresentation } from "../presentation/bidirectionalResult";

export type ResultArcPhase = "download" | "upload" | "bidirectional";

interface ResultGaugeArc {
  phase: ResultArcPhase;
  label: string;
  bytesPerSec: number;
  dashed: boolean;
}

export interface ResultGaugeHeadPlacement {
  /** Original result fraction, retained for exact endpoint mapping. */
  fraction: number;
  /** Compact inward lane; zero is reserved for the highest result. */
  lane: number;
  radius: number;
}

export interface ResultGaugeHeadLayoutOptions {
  baseRadius: number;
  arcSweep: number;
  headRadius: number;
  borderWidth: number;
  /** Clearance between outside edges of neighboring marker borders. */
  clearance?: number;
}

/** Assign stable inward lanes to sorted result heads without changing their angles. */
export function resultGaugeHeadPlacements(
  fractions: readonly number[],
  options: ResultGaugeHeadLayoutOptions,
): ResultGaugeHeadPlacement[] {
  const clearance = Math.max(0, options.clearance ?? 2);
  const extent =
    Math.max(0, options.headRadius) + Math.max(0, options.borderWidth);
  const laneStep = Math.max(1, extent * 2 + clearance);
  const minimumRadius = extent + clearance;
  const placed: ResultGaugeHeadPlacement[] = [];
  const angles = fractions.map((fraction) =>
    Number.isFinite(fraction) ? fraction * options.arcSweep : 0,
  );

  for (const [index, fraction] of fractions.entries()) {
    if (index === 0) {
      placed.push({ fraction, lane: 0, radius: options.baseRadius });
      continue;
    }

    let lane = 0;
    while (true) {
      const radius = Math.max(
        minimumRadius,
        options.baseRadius - lane * laneStep,
      );
      const angle = angles[index]!;
      const overlaps = placed.some((other, otherIndex) => {
        const otherAngle = angles[otherIndex]!;
        const x = radius * Math.cos(angle);
        const y = radius * Math.sin(angle);
        const otherX = other.radius * Math.cos(otherAngle);
        const otherY = other.radius * Math.sin(otherAngle);
        return Math.hypot(x - otherX, y - otherY) < extent * 2 + clearance;
      });
      if (!overlaps || radius <= minimumRadius) {
        placed.push({ fraction, lane, radius });
        break;
      }
      lane += 1;
    }
  }
  return placed;
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

/** Completion animation is always bounded to the gauge's normalized domain. */

export function resultGaugeArcs(result: RunResult | null): ResultGaugeArc[] {
  if (!result) return [];
  const arcs: ResultGaugeArc[] = [];
  const add = (
    phase: ResultArcPhase,
    label: string,
    bytesPerSec: number,
    dashed = false,
  ) => arcs.push({ phase, label, bytesPerSec, dashed });
  for (const { phase, label } of [
    { phase: "download", label: "Download" },
    { phase: "upload", label: "Upload" },
  ] as const) {
    const value = result[phase];
    if (value) add(phase, label, value.reportedBytesPerSec);
  }
  const bidi = bidirectionalResultPresentation(
    result.bidirectional?.down?.reportedBytesPerSec,
    result.bidirectional?.up?.reportedBytesPerSec,
  );
  if (bidi.combinedBytesPerSec != null) {
    add("bidirectional", "Bidirectional", bidi.combinedBytesPerSec);
  } else if (bidi.survivingDirection) {
    const value = bidi[bidi.survivingDirection];
    if (value != null) {
      const direction =
        bidi.survivingDirection === "down" ? "download" : "upload";
      add("bidirectional", `Bidirectional ${direction}`, value, true);
    }
  }
  return sortResultGaugeArcs(arcs);
}
