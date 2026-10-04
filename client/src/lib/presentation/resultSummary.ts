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
  LatencyBucket,
  ThroughputSample,
  RunResult,
  TransportRole,
} from "../runner/contract";
import type { MultiServerResult } from "../runner/measure";
import { bidirectionalResultPresentation } from "./bidirectionalResult";
import type { IconName } from "./icons";
import { serverName } from "./serverAppearance";
import {
  JARGON,
  LATENCY_POPULATION,
  MISSING,
  STAGE,
  reasonLabel,
} from "./vocabulary";

type SummaryStatus = "complete" | "partial" | "failed";
type LiveStatus = "active" | "recovering" | "pending" | "stopped" | "not-run";
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
type Transfer = ThroughputSample["phase"];
type CardGraphs = Partial<Record<Transfer, CardGraph>>;

/** Route each bounded series once; unchanged stages allocate no new points or lanes. */
export function buildCardGraphs(
  throughput: readonly ThroughputSample[],
  latency: readonly LatencyBucket[],
  spans: Record<Transfer, number>,
  previous: CardGraphs = {},
): Record<Transfer, CardGraph> {
  function sequence<T extends { t: number }>(
    before: T[] = [],
    value: (point: T) => number,
    create: (t: number, value: number) => T,
  ) {
    let next = before;
    let length = 0;
    return {
      add(t: number, amount: number) {
        const old = before[length];
        if (old === undefined || old.t !== t || value(old) !== amount) {
          if (next === before) next = before.slice(0, length);
          next.push(create(t, amount));
        } else if (next !== before) next.push(old);
        length++;
      },
      finish() {
        return next.length === length ? next : next.slice(0, length);
      },
    };
  }
  const stage = (key: Transfer) => ({
    lanes: Array.from({ length: key === "bidirectional" ? 2 : 1 }, (_, i) =>
      sequence(
        previous[key]?.lanes[i],
        (point) => point.v,
        (t, v) => ({ t, v }),
      ),
    ),
    latency: sequence(
      previous[key]?.latency,
      (point) => point.ms,
      (t, ms) => ({ t, ms }),
    ),
    start: Infinity,
    end: -Infinity,
  });
  const stages = {
    download: stage("download"),
    upload: stage("upload"),
    bidirectional: stage("bidirectional"),
  };
  for (const sample of throughput) {
    const key = sample.phase;
    if (
      key !== "bidirectional" &&
      sample.dir !== (key === "download" ? "down" : "up")
    )
      continue;
    const target = stages[key];
    target.lanes[key === "bidirectional" && sample.dir === "up" ? 1 : 0].add(
      sample.t,
      sample.bytesPerSec,
    );
    target.start = Math.min(target.start, sample.t);
    target.end = Math.max(target.end, sample.t);
  }
  for (const sample of latency) {
    if (sample.medianRttMs === null || !(sample.phase in stages)) continue;
    stages[sample.phase as Transfer].latency.add(sample.t, sample.medianRttMs);
  }
  const finish = (key: Transfer): CardGraph => {
    const target = stages[key];
    const lanes = target.lanes.map((lane) => lane.finish());
    const latency = target.latency.finish();
    const start = target.start === Infinity ? 0 : target.start;
    const span =
      Math.max(target.end === -Infinity ? 0 : target.end - start, spans[key]) ||
      1;
    const old = previous[key];
    return old &&
      old.start === start &&
      old.span === span &&
      old.latency === latency &&
      lanes.every((lane, i) => lane === old.lanes[i])
      ? old
      : { lanes, latency, start, span };
  };
  return {
    download: finish("download"),
    upload: finish("upload"),
    bidirectional: finish("bidirectional"),
  };
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
          units.tier ?? throughputUnitIndex(combined, units.base, units.kind),
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
    rows: [
      { label: "Jitter", value: formatLatency(jitterMs) },
      ...stability(latency.stabilityPct ?? null),
      ...added,
    ],
  };
}

/** What a transfer's load added to the idle median, on the transfer's own card. */
const addedLatency = (ms: number | null | undefined): SummaryRow[] =>
  ms == null ? [] : [{ label: "Added latency", value: `${fmtAddedMs(ms)} ms` }];

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
      ...addedLatency(evidence.added?.bidirectional),
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
          ...addedLatency(evidence.added?.[key]),
          ...noData(result.quietMs),
        ],
      },
    ];
  });
}

// Facts read the same way on every card: what the link peaked at, how steady it was, what moved, and what the
// load cost in latency; the latency card reads its idle population the same way.
const TRANSFER_FACTS = ["Peak", "Stability", "Transferred", "Added latency"];
const FACTS: Record<TransportRole, string[]> = {
  latency: ["Jitter", "Range", "Stability", "Timeouts"],
  download: TRANSFER_FACTS,
  upload: TRANSFER_FACTS,
  bidirectional: ["Stability", "Down + up", "Transferred", "Added latency"],
};
const FACT_TIPS: Record<string, string> = {
  Peak: JARGON.peak,
  Stability: JARGON.rateStability,
  Transferred: JARGON.transferred,
  "Added latency": JARGON.addedLatency,
  Jitter: JARGON.jitter,
  Range: JARGON.latencyRange,
};

/** A card's facts in every state, "—" until known, so a value arriving never moves the instrument. */
export const cardFacts = (card: SummaryCard): SummaryRow[] =>
  FACTS[card.key].map((label) => ({
    label,
    value: MISSING,
    ...card.rows.find((row) => row.label === label),
    tip:
      card.key === "latency" && label === "Stability"
        ? JARGON.latencyStability
        : FACT_TIPS[label],
  }));

/** Time without data after a stall, which the card's line carries so it never adds a row. */
export const cardNoData = (card: SummaryCard) =>
  card.rows.find((row) => row.label === "No data") ?? null;

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
    // A transfer's latency is named as the latency card names it; the Latency stage by its own name.
    line.stages.push(
      latency && failure.stage !== "latency"
        ? LATENCY_POPULATION[failure.stage].short
        : STAGE[failure.stage].label,
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
