import type { RunResult } from "../runner/contract";
import { bidirectionalResultPresentation } from "./bidirectionalResult";

export type ResultArcPhase = "download" | "upload" | "bidirectional";

interface ResultGaugeArc {
  phase: ResultArcPhase;
  label: string;
  bytesPerSec: number;
  dashed: boolean;
}

const arcValue = (value: number): number =>
  Number.isFinite(value) ? value : -Infinity;

/** Highest throughput is painted first so lower values can layer over it. */
export function sortResultGaugeArcs(
  arcs: readonly ResultGaugeArc[],
): ResultGaugeArc[] {
  return arcs
    .map((arc, index) => ({ arc, index }))
    .sort(
      (a, b) =>
        arcValue(b.arc.bytesPerSec) - arcValue(a.arc.bytesPerSec) ||
        a.index - b.index,
    )
    .map(({ arc }) => arc);
}

/** Completion animation is always bounded to the gauge's normalized domain. */
export function resultGaugeFillTarget(fractions: readonly number[]): number {
  let maximum = 0;
  for (const fraction of fractions) {
    if (Number.isFinite(fraction)) maximum = Math.max(maximum, fraction);
  }
  return Math.min(1, Math.max(0, maximum));
}

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
  const bidi = bidirectionalResultPresentation(result.bidirectional);
  if (bidi.combinedBytesPerSec != null) {
    add("bidirectional", "Bidirectional", bidi.combinedBytesPerSec);
  } else if (bidi.survivingDirection) {
    const value = bidi[bidi.survivingDirection];
    if (value) {
      const direction =
        bidi.survivingDirection === "down" ? "download" : "upload";
      add(
        "bidirectional",
        `Bidirectional ${direction}`,
        value.reportedBytesPerSec,
        true,
      );
    }
  }
  return sortResultGaugeArcs(arcs);
}
