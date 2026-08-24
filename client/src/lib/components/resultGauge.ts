import type { RunResult } from "../runner/contract";

export type ResultArcPhase = "download" | "upload" | "bidirectional";

export interface ResultGaugeArc {
  phase: ResultArcPhase;
  label: string;
  bytesPerSec: number;
  dashed: boolean;
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
  return arcs;
}
