import { compensationTooltip, type WireModel } from "../compensation";
import {
  fmtAddedMs,
  fmtBytes,
  fmtMs,
  formatLatency,
  formatRate,
  resultRate,
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
  wire?: WireRate | null;
  rows: SummaryRow[];
  accessible?: string;
}

export const CARD_ORDER = [
  "download",
  "upload",
  "bidirectional",
  "latency",
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

const stability = (pct: number | null, tip: string): SummaryRow[] =>
  pct === null
    ? []
    : [{ label: "Stability", value: `${Math.round(pct)}%`, tip }];

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
            stage,
            tip: JARGON.addedLatency,
          },
        ];
  });
  const jitter = formatLatency(jitterMs);
  const steady = card.status === "complete" ? (stabilityPct ?? null) : null;
  return {
    ...card,
    num: fmtMs(reportedMs),
    unit: "ms",
    rows: [
      { label: "Jitter", value: jitter, tip: JARGON.jitter },
      ...added,
      ...stability(steady, JARGON.latencyStability),
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
  const lane = (stage: "download" | "upload", bytesPerSec: number | null) => ({
    label: STAGE[stage].short,
    value:
      bytesPerSec === null ? "unavailable" : formatRate(bytesPerSec, units),
    stage,
  });
  const value = model.combinedBytesPerSec;
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
        JARGON.rateStability,
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
            tip: JARGON.transferred,
          },
          ...(peak == null
            ? []
            : [
                {
                  label: "Peak",
                  value: formatRate(peak, units),
                  tip: JARGON.peak,
                },
              ]),
          ...stability(
            complete ? result.stabilityPct : null,
            JARGON.rateStability,
          ),
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

/** Every row a stage's card will hold, still MISSING, so settling moves nothing. */
export function pendingRows(
  key: TransportRole,
  loaded: TransportRole[],
  multiple: boolean,
): SummaryRow[] {
  const row = (label: string, tip?: string, stage?: TransportRole) => ({
    label,
    value: MISSING,
    tip,
    stage,
  });
  if (key === "latency")
    return [
      row("Jitter", JARGON.jitter),
      ...loaded.map((stage) => row("Added", JARGON.addedLatency, stage)),
      row("Stability", JARGON.latencyStability),
      ...(multiple ? [row("Server")] : []),
    ];
  return [
    ...(key === "bidirectional"
      ? [
          row(STAGE.download.short, undefined, "download"),
          row(STAGE.upload.short, undefined, "upload"),
        ]
      : [row("Transferred", JARGON.transferred), row("Peak", JARGON.peak)]),
    row("Stability", JARGON.rateStability),
  ];
}

/** Failed server stages, live or saved, one line per server and reason: who, which stages, why; `scope` narrows to one server. */
export function serverIssues(details: MultiServerResult, scope = "") {
  const lines = new Map<
    string,
    { server: string; stages: string[]; reason: string }
  >();
  for (const failure of details.failures) {
    if (scope && failure.serverId !== scope) continue;
    const key = `${failure.serverId} ${failure.reason}`;
    if (!lines.has(key))
      lines.set(key, {
        server: serverName(details.selection, failure.serverId),
        stages: [],
        reason: reasonLabel(failure.reason),
      });
    lines
      .get(key)!
      .stages.push(
        `${STAGE[failure.stage].label}${failure.scope === "latency" ? " latency" : ""}`,
      );
  }
  return [...lines.values()].map(({ server, stages, reason }) => ({
    server,
    text: `${stages.join(", ")} · ${reason}`,
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
