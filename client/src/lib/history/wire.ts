import {
  compensationTooltip,
  type CompensationBreakdown,
  type CompensationEstimate,
} from "../compensation";
import type { HistoryRecord } from "./types";

type WireStage = "download" | "upload" | "bidirectional";
export type WireEstimates = {
  downloadBytesPerSec: number | null;
  uploadBytesPerSec: number | null;
  bidirectionalBytesPerSec: number | null;
} & (
  | { version: 1 }
  | { version: 2; breakdown: Record<WireStage, CompensationBreakdown | null> }
);

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
  const pct =
    multiplier == null ? null : `+${((multiplier - 1) * 100).toFixed(1)}%`;
  const breakdown = wire?.version === 2 ? wire.breakdown[stage] : null;
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

const isObject = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const hasOnly = (value: Record<string, unknown>, keys: readonly string[]) =>
  Object.keys(value).every((key) => keys.includes(key));
const nonnegative = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value) && value >= 0;
const source = (value: unknown) => value === "detected" || value === "fallback";
function isBreakdown(value: unknown): value is CompensationBreakdown | null {
  if (value === null) return true;
  if (
    !isObject(value) ||
    !hasOnly(value, [
      "componentCount",
      "factors",
      "transport",
      "transportSource",
      "framing",
      "mtuBytes",
      "ipVersion",
      "ipVersionSource",
    ])
  )
    return false;
  if (
    value.componentCount !== undefined &&
    (!Number.isInteger(value.componentCount) ||
      !nonnegative(value.componentCount) ||
      value.componentCount < 1 ||
      value.componentCount > 64)
  )
    return false;
  if (
    !["http1-clear", "https-tls", "http2", "http3-quic"].includes(
      typeof value.transport === "string" ? value.transport : "",
    ) ||
    !source(value.transportSource) ||
    !source(value.ipVersionSource)
  )
    return false;
  if (
    ![
      null,
      "http3-data",
      "webtransport-stream",
      "webtransport-datagram",
    ].includes(value.framing as string | null) ||
    ![4, 6].includes(value.ipVersion as number)
  )
    return false;
  if (
    !Number.isInteger(value.mtuBytes) ||
    !nonnegative(value.mtuBytes) ||
    value.mtuBytes < 1 ||
    value.mtuBytes > 65_536
  )
    return false;
  if (!Array.isArray(value.factors) || value.factors.length > 5) return false;
  const keys = new Set<string>();
  return value.factors.every((factor: unknown) => {
    if (
      !isObject(factor) ||
      !hasOnly(factor, ["key", "label", "contributionPct"])
    )
      return false;
    if (
      typeof factor.key !== "string" ||
      ![
        "application-framing",
        "tls-records",
        "ethernet",
        "ip",
        "transport",
      ].includes(factor.key) ||
      keys.has(factor.key)
    )
      return false;
    keys.add(factor.key);
    return (
      typeof factor.label === "string" &&
      factor.label.length > 0 &&
      factor.label.length <= 128 &&
      nonnegative(factor.contributionPct) &&
      factor.contributionPct <= 100
    );
  });
}

export function isWireEstimates(value: unknown): value is WireEstimates | null {
  if (value === null) return true;
  if (
    !isObject(value) ||
    !hasOnly(value, [
      "version",
      "downloadBytesPerSec",
      "uploadBytesPerSec",
      "bidirectionalBytesPerSec",
      ...(value.version === 2 ? ["breakdown"] : []),
    ])
  )
    return false;
  if (
    !["download", "upload", "bidirectional"].every(
      (stage) =>
        value[`${stage}BytesPerSec`] === null ||
        nonnegative(value[`${stage}BytesPerSec`]),
    )
  )
    return false;
  if (value.version === 1) return true;
  const breakdown = value.breakdown;
  return (
    value.version === 2 &&
    isObject(breakdown) &&
    hasOnly(breakdown, ["download", "upload", "bidirectional"]) &&
    ["download", "upload", "bidirectional"].every(
      (stage) =>
        isBreakdown(breakdown[stage]) &&
        (value[`${stage}BytesPerSec`] !== null || breakdown[stage] === null),
    )
  );
}
