import {
  compensationTooltip,
  type CompensationEstimate,
} from "../compensation";
import { fmtBytes, fmtMs } from "../format";
import type { TransportRole } from "../runner/contract";
import type { MultiServerResult } from "../servers/measurement";
import { bidirectionalResultPresentation } from "./bidirectionalResult";
import { MISSING, STAGE } from "./vocabulary";

type Band = "low" | "medium" | "high";
type Throughput = {
  reportedBytesPerSec: number;
  totalBytes: number;
  stabilityPct: number;
};
type Latency = {
  reportedMs: number;
  jitterMs: number | null;
  stabilityScore: number;
  band: Band;
};
type WireStage = "download" | "upload" | "bidirectional";
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
  latencyMeasured: boolean;
  latencySource?: string;
  wire: Partial<Record<WireStage, WireView | null>>;
}
export interface SummaryCard {
  key: TransportRole;
  label: string;
  icon: string;
  status: SummaryStatus;
  quality: { band: Band; pct: number } | null;
  num: string;
  unit: string;
  detail: string;
  jitter: string | null;
  wire: (WireView & { num: string }) | null;
}
type Rate = (bytesPerSec: number) => { num: string; unit: string };

const ORDER = ["download", "upload", "bidirectional", "latency"] as const;
const SHOWN_STATUS = new Set(["complete", "partial", "failed"]);

export const wireOverhead = (multiplier: number) =>
  `+${((multiplier - 1) * 100).toFixed(1)}%`;

export function liveWire(
  estimate: CompensationEstimate | null,
): WireView | null {
  if (!estimate || estimate.totalMultiplier < 1.005) return null;
  return {
    bytesPerSec: estimate.estimatedBytesPerSec,
    pct: wireOverhead(estimate.totalMultiplier),
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
const band = (score: number): Band =>
  score >= 0.9 ? "high" : score >= 0.75 ? "medium" : "low";

export function summaryCards(
  evidence: SummaryEvidence,
  rate: Rate,
  base: "base10" | "base2",
): SummaryCard[] {
  return ORDER.flatMap((key): SummaryCard[] => {
    const status = evidence.status[key];
    if (!status) return [];
    const card = {
      key,
      label: STAGE[key].short,
      icon: STAGE[key].icon,
      status,
      num: MISSING,
      unit: "",
      detail: "",
      jitter: null,
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
          detail: evidence.latencySource
            ? `from ${evidence.latencySource}`
            : "",
          quality:
            status === "complete"
              ? { band: latency.band, pct: latency.stabilityScore * 100 }
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
      if (result)
        card.detail = `${fmtBytes(result.totalBytes, base)} transferred`;
    }
    if (value === null) return [card];
    const shown = rate(value);
    const wire = evidence.wire[key];
    return [
      {
        ...card,
        ...shown,
        quality:
          status === "complete" && stabilityPct !== null
            ? { band: band(stabilityPct / 100), pct: stabilityPct }
            : null,
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
    latencyMeasured: !!server.latencyTarget,
    wire: {},
  };
}
