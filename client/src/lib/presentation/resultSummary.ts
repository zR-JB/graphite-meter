import { compensationTooltip, type WireModel } from "../compensation";
import { fmtAddedMs, fmtBytes, fmtMs } from "../format";
import type {
  AddedLatency,
  RunResult,
  TransportRole,
} from "../runner/contract";
import type { MultiServerResult } from "../runner/measure";
import { bidirectionalResultPresentation } from "./bidirectionalResult";
import type { IconName } from "./icons";
import { MISSING, RECEIVER_TIMED, STAGE } from "./vocabulary";

export type SummaryStatus = "complete" | "partial" | "failed";
export interface SummaryEvidence extends Pick<
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
  /** The stage a row belongs to, drawn as its icon. */
  stage?: TransportRole;
  note?: string;
}
/** A headline with its grouped secondary values; `details` opens behind the card. */
export interface SummaryCard {
  key: TransportRole;
  label: string;
  icon: IconName;
  status: SummaryStatus;
  num: string;
  unit: string;
  rows: SummaryRow[];
  details: SummaryRow[];
}
type Rate = (bytesPerSec: number) => { num: string; unit: string };

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

const withUnit = (rate: Rate, bytesPerSec: number) => {
  const { num, unit } = rate(bytesPerSec);
  return `${num} ${unit}`;
};
const stability = (pct: number | null): SummaryRow[] =>
  pct === null ? [] : [{ label: "Stability", value: `${Math.round(pct)}%` }];

/** From half a percent of overhead the estimate joins the transferred data, and its breakdown the details. */
function wire(
  model: WireModel | null | undefined,
  bytesPerSec: number,
  rate: Rate,
): [face: SummaryRow[], details: SummaryRow[]] {
  if (!model || model.totalMultiplier < 1.005) return [[], []];
  const overhead = `+${((model.totalMultiplier - 1) * 100).toFixed(1)}%`;
  const note = compensationTooltip(model).split("\n").join(" · ");
  return [
    [
      {
        label: "Wire",
        value: withUnit(rate, bytesPerSec * model.totalMultiplier),
      },
    ],
    [{ label: "Wire overhead", value: overhead, note }],
  ];
}

function latencyCard(card: SummaryCard, evidence: SummaryEvidence) {
  const latency = evidence.latency;
  if (!latency) return card;
  const { reportedMs, jitterMs } = latency;
  const added = LOADED.flatMap((stage): SummaryRow[] => {
    const ms = evidence.added?.[stage];
    return ms == null || evidence.status[stage] === "failed"
      ? []
      : [
          {
            label: "Added",
            value: `${fmtAddedMs(ms)} ms`,
            stage,
          },
        ];
  });
  const jitter = jitterMs == null ? MISSING : `${fmtMs(jitterMs)} ms`;
  const steady =
    card.status === "complete" && jitterMs != null
      ? Math.max(0, 100 * (1 - jitterMs / Math.max(reportedMs, 1)))
      : null;
  return {
    ...card,
    num: fmtMs(reportedMs),
    unit: "ms",
    rows: [{ label: "Jitter", value: jitter }, ...added],
    details: [
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
  rate: Rate,
  showWire: boolean,
) {
  const lanes = evidence.bidirectional;
  const model = bidirectionalResultPresentation(
    lanes?.down?.reportedBytesPerSec,
    lanes?.up?.reportedBytesPerSec,
  );
  const lane = (stage: "download" | "upload", bytesPerSec: number | null) => ({
    label: STAGE[stage].short,
    value: bytesPerSec === null ? "unavailable" : withUnit(rate, bytesPerSec),
    stage,
  });
  const value = model.combinedBytesPerSec;
  const rows =
    value === null && !model.survivingDirection
      ? []
      : [lane("download", model.down), lane("upload", model.up)];
  if (value === null) return { ...card, rows };
  const complete = card.status === "complete";
  const [face, details] =
    showWire && complete ? wire(lanes?.wire, value, rate) : [[], []];
  return {
    ...card,
    ...rate(value),
    rows: [...rows, ...face],
    details: [
      ...stability(
        complete && lanes?.down && lanes.up
          ? Math.min(lanes.down.stabilityPct, lanes.up.stabilityPct)
          : null,
      ),
      ...details,
    ],
  };
}

export function summaryCards(
  evidence: SummaryEvidence,
  rate: Rate,
  base: "base10" | "base2",
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
      rows: [],
      details: [],
    };
    if (key === "latency") return [latencyCard(card, evidence)];
    if (key === "bidirectional")
      return [bidirectionalCard(card, evidence, rate, showWire)];
    const result = evidence[key];
    if (!result) return [card];
    const value = result.reportedBytesPerSec;
    const complete = status === "complete";
    const [face, details] =
      showWire && complete ? wire(result.wire, value, rate) : [[], []];
    const peak = result.peakBytesPerSec;
    return [
      {
        ...card,
        ...rate(value),
        rows: [
          { label: "Transferred", value: fmtBytes(result.totalBytes, base) },
          ...face,
        ],
        details: [
          ...stability(complete ? result.stabilityPct : null),
          ...(peak == null
            ? []
            : [{ label: "Peak", value: withUnit(rate, peak) }]),
          ...(key === "upload"
            ? [{ label: "Timing", value: RECEIVER_TIMED }]
            : []),
          ...details,
        ],
      },
    ];
  });
}

/** One server's share of a run, with the statuses the run settled for that server. */
export function serverEvidence(
  details: MultiServerResult,
  id: string,
): SummaryEvidence | null {
  const server = details.servers.find((entry) => entry.server.id === id);
  if (!server) return null;
  return {
    status: shownStatus(server.stages),
    download: server.download,
    upload: server.upload,
    bidirectional: server.bidirectional,
    latency: server.latency,
    added: server.addedLatency,
  };
}
