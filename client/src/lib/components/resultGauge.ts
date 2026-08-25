import type { RunResult } from "../runner/contract";

export type ResultArcPhase = "download" | "upload" | "bidirectional";

export interface ResultGaugeArc {
  phase: ResultArcPhase;
  label: string;
  bytesPerSec: number;
  dashed: boolean;
}

/** Highest throughput is painted first so lower values can layer over it. */
export function sortResultGaugeArcs(
  arcs: readonly ResultGaugeArc[],
): ResultGaugeArc[] {
  return arcs
    .map((arc, index) => ({ arc, index }))
    .sort(
      (a, b) =>
        (Number.isFinite(b.arc.bytesPerSec) ? b.arc.bytesPerSec : -Infinity) -
          (Number.isFinite(a.arc.bytesPerSec)
            ? a.arc.bytesPerSec
            : -Infinity) || a.index - b.index,
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
  if (result.download)
    arcs.push({
      phase: "download",
      label: "Download",
      bytesPerSec: result.download.reportedBytesPerSec,
      dashed: false,
    });
  if (result.upload)
    arcs.push({
      phase: "upload",
      label: "Upload",
      bytesPerSec: result.upload.reportedBytesPerSec,
      dashed: false,
    });
  const bidi = result.bidirectional;
  if (bidi?.down && bidi.up)
    arcs.push({
      phase: "bidirectional",
      label: "Bidirectional",
      bytesPerSec: bidi.down.reportedBytesPerSec + bidi.up.reportedBytesPerSec,
      dashed: false,
    });
  else if (bidi?.down || bidi?.up)
    arcs.push({
      phase: "bidirectional",
      label: bidi.down ? "Bidirectional download" : "Bidirectional upload",
      bytesPerSec: (bidi.down ?? bidi.up)!.reportedBytesPerSec,
      dashed: true,
    });
  return sortResultGaugeArcs(arcs);
}
