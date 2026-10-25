// Models a result card, settled or running, with the same fixed facts in every state, so a value never moves a card.
import type { ThroughputResult } from "../../engine/measure/aggregate";
import type { LatencyResult, LatencySummary } from "../../engine/measure/latency";
import type { WireModel } from "../../engine/measure/overhead";
import {
  grid,
  type Cause,
  type CellStatus,
  type Grid,
  type MultiServerResult,
  type RunResult,
} from "../../engine/run/result";
import { STAGES, type Stage } from "../../engine/run/schedule";
import {
  fmtBytes,
  speedChange,
  fmtCount,
  fmtDuration,
  fmtLatencyRange,
  fmtMs,
  fmtPercent,
  fmtRate,
  fmtSpeed,
  MISSING,
  rateReading,
  rateTier,
  rateUnit,
  toRateUnit,
  type RateUnits,
} from "../../view/format";
import {
  listed,
  OUTCOME,
  POPULATION,
  reasonLabel,
  STAGE,
  STAGE_STATE,
  TIP,
  type StageState,
  type Word,
} from "../../view/words";
import { CARDS, FACT, GRID, type FactKey } from "./copy";

type CardStatus =
  | "pending"
  | "not-run"
  | "active"
  | "recovering"
  | "stopped"
  | "complete"
  | "partial"
  | "failed";

interface Fact {
  key: FactKey;
  label: string;
  value: string;
  tip: string;
}

export interface CardModel {
  stage: Stage;
  status: CardStatus;
  /** The word sits at the name's end; its dot shows the tone unless it is neutral. */
  word: Word | null;
  /** Nothing is measured yet, or the stage did not run, so the card shows a soft dash and no unit. */
  quiet: boolean;
  /** Its stage is warming up, so the figure reads the live rate, which nothing counts yet. */
  warming: boolean;
  figure: string;
  unit: string;
  /** Gives each bidirectional lane's figure in one unit, when the card shows the pair. */
  pair: { down: string; up: string } | null;
  /** The headline follows the frame clock as the glided rate, the glided round trip, or each lane's rate. */
  live: "rate" | "rtt" | "pair" | null;
  wire: { value: string; overhead: string; tip: string } | null;
  jitter: string | null;
  noData: string | null;
  facts: Fact[];
  /** A screen reader hears this unanimated live value in the headline's place. */
  spoken: string | null;
}

type Transfer = ThroughputResult & { wire?: WireModel | null };

/** The evidence a card shows, the run's or one server's share of it. */
export interface Evidence {
  /** As the result grid judges each stage. */
  status: Partial<Record<Stage, CellStatus>>;
  /** Stages every server that took part measured in full, however many did not: their stability and wire rate stand. */
  whole: ReadonlySet<Stage>;
  download: Transfer | null;
  upload: Transfer | null;
  bidirectional: {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
    wire?: WireModel | null;
  } | null;
  latency: LatencyResult | null;
  /** The latency card's spread, replies and timeouts read the idle population. */
  idle: LatencySummary | null;
}

/** The stages a judged result measured in full, or one server of it did. */
function wholeStages(table: Grid, id = ""): Set<Stage> {
  const rows = id ? table.rows.filter((row) => row.server.id === id) : table.rows;
  return new Set(
    STAGES.filter(
      (stage) =>
        table.columns[stage] !== "skipped" &&
        rows.some((row) => row.cells[stage].status === "complete") &&
        rows.every(({ cells }) =>
          ["complete", "unreached", "skipped"].includes(cells[stage].status),
        ),
    ),
  );
}

/** A finished result as the cards read every server together. */
export const runEvidence = (result: RunResult): Evidence => {
  const table = grid(result);
  return {
    ...result,
    status: table.columns,
    whole: wholeStages(table),
    idle: result.latencyByStage.latency,
  };
};

/** One server's share of a run, each stage as its cell reads; a server dropped at Start measured nothing. */
export function serverEvidence(table: Grid, details: MultiServerResult, id: string): Evidence {
  const row = table.rows.find((entry) => entry.server.id === id);
  const status = row
    ? Object.fromEntries(STAGES.map((stage) => [stage, row.cells[stage].status]))
    : {};
  const whole = wholeStages(table, id);
  const server = details.servers.find((entry) => entry.server.id === id);
  if (!server)
    return {
      status,
      whole,
      download: null,
      upload: null,
      bidirectional: null,
      latency: null,
      idle: null,
    };
  return {
    status,
    whole,
    download: server.download,
    upload: server.upload,
    bidirectional: server.bidirectional,
    latency: server.latency,
    idle: server.latencyByStage.latency,
  };
}

/** What the cards read: the lens's server, or all together; latency reads only its shown server so figures never mix
 *  servers. */
export function shownEvidence(view: {
  table: Grid | null;
  details: MultiServerResult | null;
  lens: string;
  focus: string | null;
  run: Omit<Evidence, "idle" | "whole">;
  /** The focus's live replies stand in while it has no result. */
  idle: LatencySummary | null;
}): Evidence {
  const { table, details, lens, focus, run } = view;
  if (table && details && lens) return serverEvidence(table, details, lens);
  const evidence = { ...run, whole: table ? wholeStages(table) : new Set<Stage>() };
  const focused = details?.servers.find((entry) => entry.server.id === focus);
  if (!focused?.latency) return { ...evidence, idle: view.idle };
  const own = focused.stages.latency;
  return {
    ...evidence,
    status: own === "not-run" ? run.status : { ...run.status, latency: own },
    latency: focused.latency,
    idle: focused.latencyByStage.latency,
  };
}

const SETTLED = new Set<CellStatus>(["complete", "partial", "failed"]);
export const settled = (
  status: CellStatus | null | undefined,
): status is "complete" | "partial" | "failed" => !!status && SETTLED.has(status);

const LOOK: Record<StageState, CardStatus> = {
  skipped: "not-run",
  unreached: "not-run",
  upcoming: "pending",
  warmup: "active",
  running: "active",
  recovering: "recovering",
  stopped: "stopped",
  complete: "complete",
  partial: "partial",
  failed: "failed",
};
const QUIET = new Set<CardStatus>(["pending", "not-run"]);

/** A card names its state in the words chips and History use; an upcoming or complete stage needs none. */
const wordOf = (state: StageState): Word | null =>
  state === "upcoming" || state === "complete" ? null : STAGE_STATE[state];

const TRANSFER_FACTS: readonly FactKey[] = ["peak", "stability", "transferred"];
const FACTS: Record<Stage, readonly FactKey[]> = {
  latency: ["stability", "range", "replies", "timeouts"],
  download: TRANSFER_FACTS,
  upload: TRANSFER_FACTS,
  bidirectional: ["stability", "combined", "transferred"],
};
type Known = Partial<Record<FactKey, string>>;

function facts(stage: Stage, known: Known): Fact[] {
  return FACTS[stage].map((key) => ({
    key,
    label: FACT[key].label,
    value: known[key] ?? MISSING,
    tip: stage === "latency" && key === "stability" ? TIP.latencyStability : FACT[key].tip,
  }));
}

const percent = (pct: number | null | undefined) => (pct == null ? undefined : fmtPercent(pct));

const idleFacts = (idle: LatencySummary | null): Known => ({
  range: idle ? fmtLatencyRange(idle.minMs, idle.maxMs) : undefined,
  replies: idle ? String(idle.probeCount - idle.timeoutCount) : undefined,
  timeouts: idle?.probeCount ? fmtPercent((100 * idle.timeoutCount) / idle.probeCount) : undefined,
});

/** Time without data shows from half a second. */
const noData = (ms: number | undefined) => (ms !== undefined && ms >= 500 ? fmtDuration(ms) : null);

const STACK = ["application-framing", "tls-records", "transport", "ip", "ethernet"];

/** From half a percent of overhead the wire rate sits under the headline, its layers in its tip. */
function wire(
  model: WireModel | null | undefined,
  bytesPerSec: number,
  units: RateUnits,
): CardModel["wire"] {
  if (!model || model.totalMultiplier < 1.005) return null;
  const shown = model.factors
    .filter((factor) => factor.contributionPct > 0)
    .sort(
      (a, b) =>
        STACK.indexOf(a.key) - STACK.indexOf(b.key) || b.contributionPct - a.contributionPct,
    );
  const split = new Set(shown.map((factor) => factor.key)).size < shown.length;
  const extra = (model.totalMultiplier - 1) * 100;
  return {
    value: fmtRate(bytesPerSec * model.totalMultiplier, units),
    overhead: `+${fmtPercent(extra)}`,
    tip: [
      `${CARDS.wire} +${fmtPercent(extra)} over payload`,
      ...shown.map((factor) => `${factor.label}\t+${fmtPercent(factor.contributionPct)}`),
      ...(split ? ["Each server's headers, weighted by its bytes"] : []),
      `Estimated at a ${fmtCount(model.mtuBytes)} B MTU`,
    ].join("\n"),
  };
}

/** Puts both lanes in one tier and one unit, so a single surviving lane still reads with its unit. */
function pairOf(down: number | null, up: number | null, units: RateUnits) {
  const reference = down !== null && up !== null ? down + up : (down ?? up);
  if (reference === null) return null;
  const tier = units.tier ?? rateTier(reference, units);
  const figure = (value: number | null) =>
    value === null ? MISSING : fmtSpeed(toRateUnit(value, units, tier));
  return { pair: { down: figure(down), up: figure(up) }, unit: rateUnit(units, tier) };
}

const blank = (stage: Stage, state: StageState): CardModel => {
  const status = LOOK[state];
  return {
    stage,
    status,
    word: wordOf(state),
    quiet: QUIET.has(status),
    warming: state === "warmup",
    figure: MISSING,
    unit: "",
    pair: null,
    live: null,
    wire: null,
    jitter: null,
    noData: null,
    facts: facts(stage, {}),
    spoken: null,
  };
};

export function settledCard(
  stage: Stage,
  status: "complete" | "partial" | "failed",
  evidence: Evidence,
  units: RateUnits,
  showWire: boolean,
): CardModel {
  const card = blank(stage, status);
  const complete = status === "complete" || evidence.whole.has(stage);
  if (stage === "latency") {
    const latency = evidence.latency;
    const idle = idleFacts(evidence.idle);
    if (!latency) return { ...card, facts: facts(stage, idle) };
    return {
      ...card,
      figure: fmtMs(latency.reportedMs),
      unit: "ms",
      jitter: latency.jitterMs === null ? null : `${fmtMs(latency.jitterMs)} ms`,
      facts: facts(stage, { stability: percent(latency.stabilityPct), ...idle }),
    };
  }
  if (stage === "bidirectional") {
    const lanes = evidence.bidirectional;
    const down = lanes?.down?.reportedBytesPerSec ?? null;
    const up = lanes?.up?.reportedBytesPerSec ?? null;
    const shown = pairOf(down, up, units);
    if (!shown) return card;
    const combined = down !== null && up !== null ? down + up : null;
    const moved = (lanes?.down?.totalBytes ?? 0) + (lanes?.up?.totalBytes ?? 0);
    const reading = rateReading(combined, units);
    return {
      ...card,
      figure: reading.figure,
      unit: shown.unit,
      pair: shown.pair,
      wire: showWire && complete && combined !== null ? wire(lanes?.wire, combined, units) : null,
      noData: noData(Math.max(lanes?.down?.quietMs ?? 0, lanes?.up?.quietMs ?? 0)),
      facts: facts(stage, {
        stability:
          complete && lanes?.down && lanes.up
            ? percent(Math.min(lanes.down.stabilityPct, lanes.up.stabilityPct))
            : undefined,
        combined: combined === null ? undefined : fmtRate(combined, units),
        transferred: moved ? fmtBytes(moved, units.base) : undefined,
      }),
    };
  }
  const result = evidence[stage];
  if (!result) return card;
  const reading = rateReading(result.reportedBytesPerSec, units);
  return {
    ...card,
    figure: reading.figure,
    unit: reading.unit,
    wire: showWire && complete ? wire(result.wire, result.reportedBytesPerSec, units) : null,
    noData: noData(result.quietMs),
    facts: facts(stage, {
      peak: fmtRate(result.peakBytesPerSec, units),
      stability: complete ? percent(result.stabilityPct) : undefined,
      transferred: fmtBytes(result.totalBytes, units.base),
    }),
  };
}

/** A stage that has not settled shows this, read from the run as it goes. */
export interface LiveEvidence {
  /** The card shows its own stage's live value, the latency stage measuring or the stage's own rates. */
  own: boolean;
  /** The latency stage's newest probes went unanswered. */
  lost: boolean;
  /** A screen reader hears this unanimated value while the stage runs, in bytes/s or ms; null before any. */
  spoken: number | null;
  /** A stopped stage keeps the last value its series measured. */
  held: number | null;
  idle: LatencySummary | null;
  bytes: number;
  /** Peak and Stability so far come from the stage's series by the second. */
  shape: { peak: number; stability: number | null } | null;
}

export function liveCard(
  stage: Stage,
  state: StageState,
  live: LiveEvidence,
  units: RateUnits,
): CardModel {
  const card = blank(stage, state);
  const running = card.status === "active" || card.status === "recovering";
  const stopped = card.status === "stopped";
  const reading = (value: number | null) =>
    value === null
      ? { figure: MISSING, unit: "" }
      : stage === "latency"
        ? { figure: fmtMs(value), unit: "ms" }
        : rateReading(value, units);
  const spoken = !running
    ? null
    : stage === "latency" && live.lost
      ? CARDS.probeTimeout
      : spokenOf(reading(live.spoken));
  if (!(running && live.own) && !stopped) return { ...card, spoken };
  if (stage === "latency") {
    const idle = live.idle;
    const timeout = running && live.lost;
    return {
      ...card,
      ...(stopped ? reading(live.held) : { figure: MISSING, unit: timeout ? CARDS.timeout : "ms" }),
      live: stopped || timeout ? null : "rtt",
      jitter: idle?.jitterMs == null ? null : `${fmtMs(idle.jitterMs)} ms`,
      facts: facts(stage, idleFacts(idle)),
      spoken,
    };
  }
  const known: Known = {
    // A warmup moves bytes nothing counts, so the fact waits for the window.
    transferred: card.warming ? undefined : fmtBytes(live.bytes, units.base),
    stability: percent(live.shape?.stability),
  };
  if (stage !== "bidirectional")
    known.peak = live.shape ? fmtRate(live.shape.peak, units) : undefined;
  const held = stopped
    ? reading(live.held)
    : { figure: MISSING, unit: rateUnit(units, units.tier ?? 0) };
  return {
    ...card,
    ...held,
    pair: stage === "bidirectional" && !stopped ? { down: MISSING, up: MISSING } : null,
    live: stopped ? null : stage === "bidirectional" ? "pair" : "rate",
    facts: facts(stage, known),
    spoken,
  };
}

const spokenOf = ({ figure, unit }: { figure: string; unit: string }) =>
  figure === MISSING ? null : `${figure} ${unit}`;

export interface Issue {
  server: string;
  stages: string;
  reason: string;
}

/** The causes in a grid, one line per server and reason; `scope` narrows to one server. */
export function serverIssues(table: Grid, scope = ""): Issue[] {
  const lines = new Map<string, Issue & { places: string[] }>();
  for (const { server, causes } of table.rows) {
    if (scope && server.id !== scope) continue;
    for (const cause of causes) {
      const key = `${server.id} ${cause.reason}`;
      const line = lines.get(key) ?? {
        server: server.name,
        stages: "",
        reason: reasonLabel(cause.reason),
        places: [],
      };
      lines.set(key, line);
      line.places.push(causePlace(cause));
    }
  }
  return [...lines.values()].map(({ places, ...line }) => ({ ...line, stages: places.join(", ") }));
}

export const lower = (text: string) => text.charAt(0).toLowerCase() + text.slice(1);
const loaded = (cause: Cause) => cause.scope === "latency" && cause.stage !== "latency";

/** Where a cause arose, as a line starts with it: Start, a stage, or a transfer stage's loaded latency. */
export const causePlace = (cause: Cause) =>
  cause.stage === "start"
    ? "Start"
    : loaded(cause)
      ? POPULATION[cause.stage]
      : STAGE[cause.stage].label;

/** Causes of one reason mid-sentence: "connection lost in Latency and Download", "server at capacity at Start". */
export function causeAt(...causes: Cause[]): string {
  const places = [
    ...new Set(
      causes.map((cause) => (loaded(cause) ? lower(causePlace(cause)) : causePlace(cause))),
    ),
  ];
  const stages = places.filter((place) => place !== "Start");
  const where = [
    ...(places.includes("Start") ? ["at Start"] : []),
    ...(stages.length ? [`in ${listed(stages)}`] : []),
  ];
  return `${lower(reasonLabel(causes[0]!.reason))} ${where.join(" and ")}`;
}

/** Where a server left the run, and why: "Left in Download: connection lost". */
export const leftLine = (cause: Cause) =>
  `${cause.stage === "start" ? "Left at Start" : `Left in ${STAGE[cause.stage].label}`}: ${lower(reasonLabel(cause.reason))}`;

/** A card's line: each server's failure in its stage, then who never reached it; one server alone says why. */
export function cardLine(table: Grid, stage: Stage, lens = ""): string {
  const attributed = !lens && table.rows.length > 1;
  const rows = lens ? table.rows.filter((row) => row.server.id === lens) : table.rows;
  const lines: string[] = [];
  // Servers short for one reason share it: "Frankfurt and Oslo: connection lost".
  const short = new Map<string, string[]>();
  const without: string[] = [];
  for (const { server, cells } of rows) {
    const { status, cause } = cells[stage];
    if (status === "unreached") {
      if (attributed) without.push(server.name);
      else if (cause) lines.push(leftLine(cause));
    } else if ((status === "partial" || status === "failed") && cause) {
      const reason = reasonLabel(cause.reason);
      if (attributed) short.set(reason, [...(short.get(reason) ?? []), server.name]);
      else lines.push(reason);
    }
  }
  for (const [reason, names] of short) lines.push(`${listed(names)}: ${lower(reason)}`);
  if (without.length) lines.push(`${lines.length ? "without" : "Without"} ${listed(without)}`);
  return lines.join("; ");
}

/** A card's speed changes, each server's as its live rate confirmed them; several servers are named, and the
 *  bidirectional stage names each lane. */
export function changeLine(
  details: MultiServerResult | null,
  stage: Stage,
  lens: string,
  units: RateUnits,
): string {
  if (stage === "latency" || !details) return "";
  const named = !lens && details.servers.length > 1;
  const lines = details.servers.flatMap(({ server, ...own }) => {
    if (lens && server.id !== lens) return [];
    const lanes =
      stage === "bidirectional"
        ? ([
            ["download", own.bidirectional?.down],
            ["upload", own.bidirectional?.up],
          ] as const)
        : ([[stage, own[stage]]] as const);
    return lanes.flatMap(([lane, result]) =>
      (result?.changes ?? []).map((change) => {
        const text = speedChange(change, units);
        const said = stage === "bidirectional" ? `${STAGE[lane].label} ${lower(text)}` : text;
        return named ? `${server.name}: ${lower(said)}` : said;
      }),
    );
  });
  return lines.map((line, index) => (index && !named ? lower(line) : line)).join("; ");
}

/** Speaks a completed run once in card order, with each headline, its status if not complete and latency's jitter. */
export function resultSentence(cards: readonly CardModel[]): string {
  return cards
    .map((card) => {
      const headline = card.pair
        ? `${STAGE.download.short} ${card.pair.down}, ${STAGE.upload.short} ${card.pair.up} ${card.unit}`
        : `${card.figure} ${card.unit}`.trim();
      const status = card.word ? `, ${card.word.label.toLowerCase()}` : "";
      const jitter = card.jitter ? `, jitter ${card.jitter}` : "";
      return `${STAGE[card.stage].label} ${headline}${status}${jitter}`;
    })
    .join("; ");
}

export interface GridCell {
  stage: Stage;
  /** The server's own value, or the cell's word when it has none. */
  text: string;
  measured: boolean;
  /** Partial and Failed carry a dot of their tone; Complete and Not reached none. */
  word: Word | null;
  tip: string;
}
export interface GridNote {
  tone: "err" | "warn";
  /** The cause's sentence, which names its server and stage, then what it cost. */
  text: string;
}
export interface GridView {
  stages: Stage[];
  /** Each column's unit, shared by its cells. */
  units: Partial<Record<Stage, string>>;
  rows: { id: string; name: string; word: Word | null; cells: GridCell[] }[];
  notes: GridNote[];
}

/** The result grid in words: every server's value or word per planned stage, then each cause and what it cost. */
export function gridView(result: RunResult, table: Grid, units: RateUnits): GridView {
  const stages = STAGES.filter((stage) => table.columns[stage] !== "skipped");
  const tier = units.tier ?? 0;
  const value = (stage: Stage, id: string): string => {
    const server = result.multiServer.servers.find((entry) => entry.server.id === id);
    if (stage === "latency") return server?.latency ? fmtMs(server.latency.reportedMs) : MISSING;
    const lanes =
      stage === "bidirectional"
        ? [server?.bidirectional?.down, server?.bidirectional?.up]
        : [server?.[stage]];
    if (!lanes.every(Boolean)) return MISSING;
    const bytesPerSec = lanes.reduce((sum, lane) => sum + lane!.reportedBytesPerSec, 0);
    return fmtSpeed(toRateUnit(bytesPerSec, units, tier));
  };
  const rows = table.rows.map(({ server, cells, outcome }) => ({
    id: server.id,
    name: server.name,
    word: outcome === "complete" ? null : OUTCOME[outcome],
    cells: stages.map((stage): GridCell => {
      const { status, cause } = cells[stage];
      const shown = status === "complete" || status === "partial";
      const word = status === "complete" ? null : STAGE_STATE[status];
      // A server that left says where; one that failed here says what happened.
      const why = !cause ? [] : status === "unreached" ? [leftLine(cause)] : [cause.message];
      return {
        stage,
        text: shown ? value(stage, server.id) : word!.label,
        measured: shown,
        word: status === "unreached" ? null : word,
        tip: [word?.label, ...why].filter(Boolean).join("\n"),
      };
    }),
  }));
  // One server's cards already say each cause of their stage; its list keeps only what no card shows.
  const notes = table.rows.flatMap(({ cells, causes }) =>
    causes
      .filter((cause) => table.rows.length > 1 || loaded(cause))
      .map((cause): GridNote => {
        const after = stages.filter(
          (stage) => cells[stage].status === "unreached" && cells[stage].cause === cause,
        );
        const failed = stages.some(
          (stage) => cells[stage].status === "failed" && cells[stage].cause === cause,
        );
        return {
          tone: failed ? "err" : "warn",
          text: [
            cause.message,
            after.length ? GRID.unreached(after.map((stage) => STAGE[stage].label)) : "",
          ]
            .filter(Boolean)
            .join(" "),
        };
      }),
  );
  return {
    stages,
    units: Object.fromEntries(
      stages.map((stage) => [stage, stage === "latency" ? "ms" : rateUnit(units, tier)]),
    ),
    rows,
    notes,
  };
}
