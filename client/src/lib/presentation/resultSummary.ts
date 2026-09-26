import {
  compensationTooltip,
  type CompensationEstimate,
} from "../compensation";
import { fmtAddedMs, fmtBytes, fmtMs } from "../format";
import type { TransportRole } from "../runner/contract";
import type { MultiServerResult } from "../runner/measure";
import { bidirectionalResultPresentation } from "./bidirectionalResult";
import type { IconName } from "./icons";
import { MISSING, RECEIVER_TIMED, STAGE } from "./vocabulary";

type Band = "low" | "medium" | "high";
type Throughput = {
  reportedBytesPerSec: number;
  peakBytesPerSec?: number | null;
  totalBytes: number;
  stabilityPct: number;
};
type Latency = { reportedMs: number; jitterMs: number | null };
type WireStage = "download" | "upload" | "bidirectional";
type Added = {
  addedMs?: Partial<Record<WireStage, number | null>>;
  grade: string;
};
export type SummaryStatus = "complete" | "partial" | "failed";
export interface WireView {
  bytesPerSec: number;
  pct: string | null;
  tooltip: string;
}
export interface SummaryEvidence {
  status: Partial<Record<TransportRole, SummaryStatus>>;
  download: Throughput | null;
  upload: Throughput | null;
  bidirectional: { down: Throughput | null; up: Throughput | null } | null;
  latency: Latency | null;
  added: Added | null;
  latencyMeasured: boolean;
  latencySource?: string;
  wire: Partial<Record<WireStage, WireView | null>>;
}
export interface SummaryCard {
  key: TransportRole;
  label: string;
  icon: IconName;
  status: SummaryStatus;
  quality: { band: Band; pct: number } | null;
  num: string;
  unit: string;
  detail: string;
  jitter: string | null;
  /** Signed added latency for a loaded stage, or the latency card's secondary grade. */
  added: string | null;
  grade: string | null;
  wire: (WireView & { num: string }) | null;
}
type Rate = (bytesPerSec: number) => { num: string; unit: string };

const ORDER = ["download", "upload", "bidirectional", "latency"] as const;
const SHOWN_STATUS = new Set(["complete", "partial", "failed"]);

export const wireOverhead = (multiplier: number) =>
  multiplier < 1.005 ? null : `+${((multiplier - 1) * 100).toFixed(1)}%`;

export function liveWire(
  estimate: CompensationEstimate | null,
): WireView | null {
  const pct = estimate && wireOverhead(estimate.totalMultiplier);
  if (!estimate || !pct) return null;
  return {
    bytesPerSec: estimate.estimatedBytesPerSec,
    pct,
    tooltip: compensationTooltip(estimate),
  };
}

/** The shown server's evidence, else the run's own; stages without a result are left out. */
export function summaryEvidence(
  stages: Record<TransportRole, string>,
  run: Omit<SummaryEvidence, "status" | "latencyMeasured" | "latencySource">,
  details: MultiServerResult | null | undefined,
  shown: string,
  latencyFocus: string | null | undefined,
): SummaryEvidence {
  const status = Object.fromEntries(
    Object.entries(stages).filter(([, value]) => SHOWN_STATUS.has(value)),
  ) as SummaryEvidence["status"];
  const multiple = details && details.selection.length > 1;
  return (
    (details && shown && serverEvidence(details, shown, status)) || {
      ...run,
      status,
      latencyMeasured: true,
      latencySource: multiple
        ? details.selection.find((server) => server.id === latencyFocus)?.name
        : undefined,
    }
  );
}
const inUnit = (rate: Rate, bytesPerSec: number, shown: string) => {
  const { num, unit } = rate(bytesPerSec);
  return unit === shown ? num : `${num} ${unit}`;
};
/** One pip for every stage: 100 × (1 − relative variation), banded at 90 and 75. */
const quality = (pct: number | null) =>
  pct === null
    ? null
    : {
        band: (pct >= 90 ? "high" : pct >= 75 ? "medium" : "low") as Band,
        pct,
      };

export function summaryCards(
  evidence: SummaryEvidence,
  rate: Rate,
  base: "base10" | "base2",
): SummaryCard[] {
  return ORDER.flatMap((key): SummaryCard[] => {
    const status = evidence.status[key];
    if (!status) return [];
    const card: SummaryCard = {
      key,
      label: STAGE[key].short,
      icon: STAGE[key].icon,
      status,
      num: MISSING,
      unit: "",
      detail: "",
      jitter: null,
      added: null,
      grade: null,
      quality: null,
      wire: null,
    };
    if (key === "latency") {
      const latency = evidence.latency;
      if (!evidence.latencyMeasured)
        return [{ ...card, status: "complete", detail: "Not measured" }];
      if (!latency) return [card];
      return [
        {
          ...card,
          num: fmtMs(latency.reportedMs),
          unit: "ms",
          jitter: latency.jitterMs == null ? MISSING : fmtMs(latency.jitterMs),
          grade: evidence.added ? `Grade ${evidence.added.grade}` : null,
          detail: evidence.latencySource
            ? `from ${evidence.latencySource}`
            : "",
          quality:
            status === "complete" && latency.jitterMs != null
              ? quality(
                  Math.max(
                    0,
                    100 *
                      (1 - latency.jitterMs / Math.max(latency.reportedMs, 1)),
                  ),
                )
              : null,
        },
      ];
    }
    let value: number | null;
    let stabilityPct: number | null;
    if (key === "bidirectional") {
      const lanes = evidence.bidirectional;
      const model = bidirectionalResultPresentation(
        lanes?.down?.reportedBytesPerSec,
        lanes?.up?.reportedBytesPerSec,
      );
      value = model.combinedBytesPerSec;
      stabilityPct =
        lanes?.down && lanes.up
          ? Math.min(lanes.down.stabilityPct, lanes.up.stabilityPct)
          : null;
      const unit = value === null ? "" : rate(value).unit;
      const down = inUnit(rate, model.down ?? 0, unit);
      const up = inUnit(rate, model.up ?? 0, unit);
      card.detail =
        value !== null
          ? `↓ ${down} ↑ ${up}`
          : model.survivingDirection === "down"
            ? `↓ ${down} · upload unavailable`
            : model.survivingDirection === "up"
              ? `↑ ${up} · download unavailable`
              : "";
    } else {
      const result = evidence[key];
      value = result?.reportedBytesPerSec ?? null;
      stabilityPct = result?.stabilityPct ?? null;
      if (result) {
        const { unit } = rate(result.reportedBytesPerSec);
        const peak = result.peakBytesPerSec;
        card.detail = [
          ...(peak == null ? [] : [`peak ${inUnit(rate, peak, unit)}`]),
          `${fmtBytes(result.totalBytes, base)} transferred`,
          ...(key === "upload" ? [RECEIVER_TIMED] : []),
        ].join(" · ");
      }
    }
    const added = evidence.added?.addedMs?.[key];
    if (added != null) card.added = fmtAddedMs(added);
    if (value === null) return [card];
    const shown = rate(value);
    const wire = evidence.wire[key];
    return [
      {
        ...card,
        ...shown,
        quality: status === "complete" ? quality(stabilityPct) : null,
        wire:
          wire && status === "complete"
            ? { ...wire, num: inUnit(rate, wire.bytesPerSec, shown.unit) }
            : null,
      },
    ];
  });
}

/** One server's share of a run; its own failures decide each status. */
export function serverEvidence(
  details: MultiServerResult,
  id: string,
  status: SummaryEvidence["status"],
): SummaryEvidence | null {
  const server = details.servers.find((entry) => entry.server.id === id);
  if (!server) return null;
  const scoped = (key: TransportRole, base: SummaryStatus): SummaryStatus => {
    if (base === "failed") return base;
    if (key === "latency" && !server.latencyTarget) return "complete";
    const measured =
      key === "bidirectional"
        ? !!(server.bidirectional?.down || server.bidirectional?.up)
        : !!server[key];
    const failed = details.failures.some(
      (failure) =>
        failure.serverId === id &&
        failure.stage === key &&
        failure.scope === (key === "latency" ? "latency" : "throughput"),
    );
    if (measured) return failed ? "partial" : "complete";
    return failed ? "failed" : "partial";
  };
  return {
    status: Object.fromEntries(
      ORDER.flatMap((key) => {
        const base = status[key];
        return base ? [[key, scoped(key, base)]] : [];
      }),
    ),
    download: server.download,
    upload: server.upload,
    bidirectional: server.bidirectional,
    latency: server.latency,
    added: server.bufferbloat,
    latencyMeasured: !!server.latencyTarget,
    wire: {},
  };
}
