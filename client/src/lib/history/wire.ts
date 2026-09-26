import {
  compensationTooltip,
  type CompensationBreakdown,
  type CompensationEstimate,
} from "../compensation";
import { wireOverhead } from "../presentation/resultSummary";
import type { HistoryRecord } from "./types";

type WireStage = "download" | "upload" | "bidirectional";
export interface WireEstimates {
  version: 2;
  breakdown: Record<WireStage, CompensationBreakdown | null>;
  downloadBytesPerSec: number | null;
  uploadBytesPerSec: number | null;
  bidirectionalBytesPerSec: number | null;
}

/** Save the model that produced the estimate; historical paths never use today's connection. */
export function historyWireEstimates(
  download: CompensationEstimate | null,
  upload: CompensationEstimate | null,
  bidirectional: CompensationEstimate | null,
): WireEstimates | null {
  if (!download && !upload && !bidirectional) return null;
  const snapshot = (
    estimate: CompensationEstimate | null,
  ): CompensationBreakdown | null => {
    if (!estimate) return null;
    const {
      measuredBytesPerSec,
      estimatedBytesPerSec,
      lowerBytesPerSec,
      upperBytesPerSec,
      totalMultiplier,
      confidence,
      ...model
    } = estimate;
    return structuredClone(model);
  };
  return {
    version: 2,
    downloadBytesPerSec: download?.estimatedBytesPerSec ?? null,
    uploadBytesPerSec: upload?.estimatedBytesPerSec ?? null,
    bidirectionalBytesPerSec: bidirectional?.estimatedBytesPerSec ?? null,
    breakdown: {
      download: snapshot(download),
      upload: snapshot(upload),
      bidirectional: snapshot(bidirectional),
    },
  };
}

export function historyWirePresentation(
  record: HistoryRecord,
  stage: WireStage,
) {
  const wire = record.wireEstimates;
  const bytesPerSec = wire?.[`${stage}BytesPerSec`];
  if (bytesPerSec == null) return null;
  const measured =
    stage === "bidirectional"
      ? record.stages.bidirectional.down && record.stages.bidirectional.up
        ? record.stages.bidirectional.down.reportedBytesPerSec +
          record.stages.bidirectional.up.reportedBytesPerSec
        : null
      : record.stages[stage].result?.reportedBytesPerSec;
  // A saved combined estimate cannot be attributed to one surviving lane.
  if (measured == null) return null;
  const multiplier =
    measured && bytesPerSec >= measured ? bytesPerSec / measured : null;
  const pct = multiplier == null ? null : wireOverhead(multiplier);
  const breakdown = wire?.breakdown[stage];
  return {
    bytesPerSec,
    pct,
    tooltip:
      breakdown && multiplier != null
        ? compensationTooltip(breakdown)
        : breakdown
          ? "Overhead percentage unavailable."
          : "Per-part breakdown unavailable.",
  };
}
