import { compensationTooltip, type WireModel } from "../compensation";
import {
  fmtAddedMs,
  fmtBytes,
  fmtDuration,
  fmtMs,
  formatLatency,
  formatRate,
  resultRate,
  throughputUnitIndex,
  type RateUnits,
} from "../format";
import type {
  AddedLatency,
  RunResult,
  TransportRole,
} from "../runner/contract";
import type { MultiServerResult } from "../runner/measure";
import { bidirectionalResultPresentation } from "./bidirectionalResult";
import type { IconName } from "./icons";
import { serverName } from "./serverAppearance";
import { JARGON, MISSING, STAGE, reasonLabel } from "./vocabulary";

type SummaryStatus = "complete" | "partial" | "failed";
type LiveStatus = "active" | "pending" | "stopped" | "not-run";
interface SummaryEvidence extends Pick<
  RunResult,
  "download" | "upload" | "bidirectional" | "latency"
> {
  status: Partial<Record<TransportRole, SummaryStatus>>;
  added: AddedLatency | null;
}
export interface SummaryRow {
  label: string;
  value: string;
  stage?: TransportRole;
  short?: string;
  tip?: string;
}
interface WireRate {
  value: string;
  overhead: string;
  tip: string;
}
export interface SummaryCard {
  key: TransportRole;
  label: string;
  icon: IconName;
  status: SummaryStatus | LiveStatus;
  num: string;
  unit: string;
  tip: string;
  wire?: WireRate;
  rows: SummaryRow[];
  accessible?: string;
  graph?: CardGraph | null;
}
/** One stage's series on the run's timeline; the leading edge while it runs. */
export interface CardGraph {
  lanes: TracePoint[][];
  latency: { t: number; ms: number }[];
  start: number;
  span: number;
}
/** Shared by every card: rate and latency ceilings, the idle floor, and how a rate reads. */
export interface CardScale {
  ceiling: number;
  baseline: number | null;
  latencyTop: number;
  rate: (bytesPerSec: number) => string;
}

/** The stage track's order, so each chip sits under its stage. */
export const CARD_ORDER = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const;
const LOADED = ["download", "upload", "bidirectional"] as const;
const SHOWN_STATUS = new Set(["complete", "partial", "failed"]);
const shownStatus = (stages: Record<TransportRole, string>) =>
  Object.fromEntries(
    Object.entries(stages).filter(([, value]) => SHOWN_STATUS.has(value)),
  ) as SummaryEvidence["status"];

/** The shown server's evidence, else the run's own; stages without a result are left out. */
export function summaryEvidence(
  stages: Record<TransportRole, string>,
  run: Omit<SummaryEvidence, "status">,
  details: MultiServerResult | null | undefined,
  shown: string,
): SummaryEvidence {
  return (
    (details && shown && serverEvidence(details, shown)) || {
      ...run,
      status: shownStatus(stages),
    }
  );
}

export const laneShort = (
  bytesPerSec: number | null | undefined,
  combined: number | null | undefined,
  units: RateUnits,
) =>
  bytesPerSec == null
    ? MISSING
    : combined == null
      ? formatRate(bytesPerSec, units)
      : resultRate(
          bytesPerSec,
          units,
          throughputUnitIndex(combined, units.base, units.kind),
        ).num;

const stability = (pct: number | null): SummaryRow[] =>
  pct === null ? [] : [{ label: "Stability", value: `${Math.round(pct)}%` }];

/** Time without data shows from half a second. */
const noData = (ms = 0): SummaryRow[] =>
  ms < 500
    ? []
    : [{ label: "No data", value: fmtDuration(ms), tip: JARGON.noData }];

/** From half a percent of overhead the wire estimate sits under the headline. */
function wire(
  model: WireModel | null | undefined,
  bytesPerSec: number,
  units: RateUnits,
): WireRate | undefined {
  if (!model || model.totalMultiplier < 1.005) return undefined;
  const overhead = `+${((model.totalMultiplier - 1) * 100).toFixed(1)}%`;
  return {
    value: formatRate(bytesPerSec * model.totalMultiplier, units),
    overhead,
    tip: `Wire rate +${((model.totalMultiplier - 1) * 100).toFixed(2)}%\nPayload plus the headers the link also carried\n${compensationTooltip(model)}`,
  };
}

function latencyCard(card: SummaryCard, evidence: SummaryEvidence) {
  const latency = evidence.latency;
  if (!latency) return card;
  const { reportedMs, jitterMs } = latency;
  const added = LOADED.flatMap((stage): SummaryRow[] => {
    const ms = evidence.added?.[stage];
    return ms == null
      ? []
      : [{ label: "Added", value: `${fmtAddedMs(ms)} ms`, stage }];
  });
  return {
    ...card,
    num: fmtMs(reportedMs),
    unit: "ms",
    rows: [{ label: "Jitter", value: formatLatency(jitterMs) }, ...added],
  };
}

function bidirectionalCard(
  card: SummaryCard,
  evidence: SummaryEvidence,
  units: RateUnits,
  showWire: boolean,
) {
  const lanes = evidence.bidirectional;
  const model = bidirectionalResultPresentation(
    lanes?.down?.reportedBytesPerSec,
    lanes?.up?.reportedBytesPerSec,
  );
  const value = model.combinedBytesPerSec;
  const lane = (stage: "download" | "upload", bytesPerSec: number | null) => ({
    label: STAGE[stage].short,
    value:
      bytesPerSec === null ? "unavailable" : formatRate(bytesPerSec, units),
    short: laneShort(bytesPerSec, value, units),
    stage,
  });
  const rows =
    value === null && !model.survivingDirection
      ? []
      : [lane("download", model.down), lane("upload", model.up)];
  if (value === null) return { ...card, rows };
  const complete = card.status === "complete";
  const moved = (lanes?.down?.totalBytes ?? 0) + (lanes?.up?.totalBytes ?? 0);
  return {
    ...card,
    ...resultRate(value, units),
    wire: showWire && complete ? wire(lanes?.wire, value, units) : undefined,
    rows: [
      ...rows,
      { label: "Down + up", value: formatRate(value, units) },
      ...(moved
        ? [{ label: "Transferred", value: fmtBytes(moved, units.base) }]
        : []),
      ...stability(
        complete && lanes?.down && lanes.up
          ? Math.min(lanes.down.stabilityPct, lanes.up.stabilityPct)
          : null,
      ),
      ...noData(Math.max(lanes?.down?.quietMs ?? 0, lanes?.up?.quietMs ?? 0)),
    ],
  };
}

export function summaryCards(
  evidence: SummaryEvidence,
  units: RateUnits,
  showWire: boolean,
): SummaryCard[] {
  return CARD_ORDER.flatMap((key): SummaryCard[] => {
    const status = evidence.status[key];
    if (!status) return [];
    const card: SummaryCard = {
      key,
      label: STAGE[key].short,
      icon: STAGE[key].icon,
      status,
      num: MISSING,
      unit: "",
      tip: JARGON[key],
      rows: [],
    };
    if (key === "latency") return [latencyCard(card, evidence)];
    if (key === "bidirectional")
      return [bidirectionalCard(card, evidence, units, showWire)];
    const result = evidence[key];
    if (!result) return [card];
    const value = result.reportedBytesPerSec;
    const complete = status === "complete";
    const peak = result.peakBytesPerSec;
    return [
      {
        ...card,
        ...resultRate(value, units),
        wire:
          showWire && complete ? wire(result.wire, value, units) : undefined,
        rows: [
          {
            label: "Transferred",
            value: fmtBytes(result.totalBytes, units.base),
          },
          ...(peak == null
            ? []
            : [{ label: "Peak", value: formatRate(peak, units) }]),
          ...stability(complete ? result.stabilityPct : null),
          ...noData(result.quietMs),
        ],
      },
    ];
  });
}

/** A completed run spoken in card order: each headline, then latency's jitter and added latency. */
export const resultSentence = (cards: SummaryCard[]) =>
  cards
    .map((card) =>
      [
        `${card.label} ${`${card.num} ${card.unit}`.trim()}${card.status === "complete" ? "" : `, ${card.status}`}`,
        ...(card.key === "latency"
          ? card.rows
              .filter((row) => row.label === "Jitter" || row.stage)
              .map(
                (row) =>
                  `${row.label}${row.stage ? ` ${STAGE[row.stage].short}` : ""} ${row.value}`,
              )
          : []),
      ].join(", "),
    )
    .join("; ");

export const cardTip = (card: SummaryCard) =>
  [
    card.label,
    card.tip.split("\n")[1],
    ...card.rows
      .filter((row) => row.value !== MISSING)
      .map((row) =>
        row.stage && row.label !== STAGE[row.stage].short
          ? `${row.label} under ${STAGE[row.stage].short.toLowerCase()}\t${row.value}`
          : `${row.label}\t${row.value}`,
      ),
  ].join("\n");

interface TracePoint {
  t: number;
  v: number;
}

/** Failed server stages, live or saved, one line per server and reason: who, which stages, why; `scope` narrows to one server. */
export function serverIssues(details: MultiServerResult, scope = "") {
  const lines = new Map<
    string,
    {
      server: string;
      stages: string[];
      reason: string;
      throughput: TransportRole[];
    }
  >();
  for (const failure of details.failures) {
    if (scope && failure.serverId !== scope) continue;
    const key = `${failure.serverId} ${failure.reason}`;
    if (!lines.has(key))
      lines.set(key, {
        server: serverName(details.selection, failure.serverId),
        stages: [],
        reason: reasonLabel(failure.reason),
        throughput: [],
      });
    const line = lines.get(key)!;
    const latency = failure.scope === "latency";
    line.stages.push(
      `${STAGE[failure.stage].label}${latency ? " latency" : ""}`,
    );
    if (!latency) line.throughput.push(failure.stage);
  }
  return [...lines.values()].map(({ stages, ...line }) => ({
    ...line,
    stages: stages.join(", "),
  }));
}

/** One server's share of a run, with the statuses the run settled for that server; none if it measured nothing. */
function serverEvidence(
  details: MultiServerResult,
  id: string,
): SummaryEvidence {
  const server = details.servers.find((entry) => entry.server.id === id);
  if (!server)
    return {
      status: {},
      download: null,
      upload: null,
      bidirectional: null,
      latency: null,
      added: null,
    };
  return {
    status: shownStatus(server.stages),
    download: server.download,
    upload: server.upload,
    bidirectional: server.bidirectional,
    latency: server.latency,
    added: server.addedLatency,
  };
}
