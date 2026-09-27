import { compensationTooltip, type WireModel } from "../compensation";
import {
  fmtAddedMs,
  fmtBytes,
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
  latencySource?: string;
}
export interface SummaryRow {
  label: string;
  value: string;
  stage?: TransportRole;
  short?: string;
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
  trace?: Trace | null;
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
  run: Omit<SummaryEvidence, "status" | "latencySource">,
  details: MultiServerResult | null | undefined,
  shown: string,
  latencyFocus: string | null | undefined,
): SummaryEvidence {
  const multiple = details && details.selection.length > 1;
  return (
    (details && shown && serverEvidence(details, shown)) || {
      ...run,
      status: shownStatus(stages),
      latencySource: multiple
        ? details.selection.find((server) => server.id === latencyFocus)?.name
        : undefined,
    }
  );
}

/** Under "added", whole milliseconds from 10 ms and no plus, so three stages fit a line; the hover keeps both. */
const addedShort = (ms: number) =>
  Math.abs(ms) < 9.95
    ? fmtAddedMs(ms).replace("+", "")
    : `${Math.round(ms)}`.replace("-", "−");

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
  const { reportedMs, jitterMs, stabilityPct } = latency;
  const added = LOADED.flatMap((stage): SummaryRow[] => {
    const ms = evidence.added?.[stage];
    return ms == null
      ? []
      : [
          {
            label: "Added",
            value: `${fmtAddedMs(ms)} ms`,
            short: addedShort(ms),
            stage,
          },
        ];
  });
  const steady = card.status === "complete" ? (stabilityPct ?? null) : null;
  return {
    ...card,
    num: fmtMs(reportedMs),
    unit: "ms",
    rows: [
      { label: "Jitter", value: formatLatency(jitterMs) },
      ...added,
      ...stability(steady),
      ...(evidence.latencySource
        ? [{ label: "Server", value: evidence.latencySource }]
        : []),
    ],
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
  return {
    ...card,
    ...resultRate(value, units),
    wire: showWire && complete ? wire(lanes?.wire, value, units) : undefined,
    rows: [
      ...rows,
      ...stability(
        complete && lanes?.down && lanes.up
          ? Math.min(lanes.down.stabilityPct, lanes.up.stabilityPct)
          : null,
      ),
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

export interface CardLine {
  label: string;
  tip?: string;
  facts: { value: string; stage?: TransportRole }[];
  mark?: { text: string; tip: string };
}
export function cardLine(card: SummaryCard): CardLine | null {
  if (card.wire)
    return {
      label: "wire",
      facts: [{ value: card.wire.value }],
      mark: { text: card.wire.overhead, tip: card.wire.tip },
    };
  const shown = card.rows.filter((row) => row.value !== MISSING);
  const added = shown.filter((row) => row.label === "Added");
  if (added.length)
    return {
      label: "added",
      tip: JARGON.addedLatency,
      facts: added.map((row) => ({ value: row.short!, stage: row.stage })),
    };
  const lanes = card.rows.filter((row) => row.short);
  if (lanes.length)
    return {
      label: "",
      facts: lanes.map((row) => ({ value: row.short!, stage: row.stage })),
    };
  const fact = shown.find((row) =>
    ["Jitter", "Transferred"].includes(row.label),
  );
  return fact
    ? {
        label: fact.label.toLowerCase(),
        tip: fact.label === "Jitter" ? JARGON.jitter : JARGON.transferred,
        facts: [{ value: fact.value }],
      }
    : null;
}

interface TracePoint {
  t: number;
  v: number;
}
const TRACE_W = 100;
const TRACE_H = 32;
/** Lanes binned over the stage's planned time and summed; the floor rises to the lowest bin, at most to three quarters of the peak. */
export function tracePaths(
  series: TracePoint[][],
  plannedMs: number,
  columns = 36,
) {
  const all = series.flat();
  if (all.length < 2) return null;
  const start = Math.min(...all.map((point) => point.t));
  const span =
    Math.max(plannedMs, Math.max(...all.map((point) => point.t)) - start) || 1;
  const bins = new Float64Array(columns);
  const seen = new Uint8Array(columns);
  for (const [lane, points] of series.entries()) {
    const sums = new Float64Array(columns);
    const counts = new Uint16Array(columns);
    for (const { t, v } of points) {
      const i = Math.min(
        columns - 1,
        Math.floor(((t - start) / span) * columns),
      );
      sums[i] += v;
      counts[i]++;
    }
    for (let i = 0; i < columns; i++)
      if (counts[i]) {
        bins[i] += sums[i] / counts[i];
        seen[i] |= 1 << lane;
      }
  }
  const full = (1 << series.length) - 1;
  const kept = [...bins.keys()].filter((i) => seen[i] === full);
  if (kept.length < 2) return null;
  const values = kept.map((i) => bins[i]);
  const top = Math.max(...values);
  const floor = Math.min(...values, top * 0.75);
  const points = kept.map((i) => [
    ((i + 0.5) / columns) * TRACE_W,
    TRACE_H - 3 - ((bins[i] - floor) / (top - floor || 1)) * (TRACE_H - 7),
  ]);
  const mid = (a: number[], b: number[]) =>
    `${(a[0] + b[0]) / 2} ${(a[1] + b[1]) / 2}`;
  const [x0, y0] = points[0];
  const [xn, yn] = points.at(-1)!;
  const line = `M${x0} ${y0} L${mid(points[0], points[1])} ${points
    .slice(1, -1)
    .map((p, i) => `Q${p[0]} ${p[1]} ${mid(p, points[i + 2])}`)
    .join(" ")} L${xn} ${yn}`;
  return {
    line,
    area: `${line} L${xn} ${TRACE_H + 1} L${x0} ${TRACE_H + 1} Z`,
    head: { x: xn / TRACE_W, y: yn / TRACE_H },
  };
}
export type Trace = NonNullable<ReturnType<typeof tracePaths>>;

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
