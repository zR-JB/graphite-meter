// Models a result card, settled or running, with the same fixed facts in every state, so a value never moves a card.
import type { ThroughputResult } from "../../engine/measure/aggregate";
import type { LatencyResult, LatencySummary } from "../../engine/measure/latency";
import type { WireModel } from "../../engine/measure/overhead";
import { stabilityPct } from "../../engine/measure/stability";
import type { MultiServerResult, StageStatus } from "../../engine/run/result";
import type { Stage, TransferStage } from "../../engine/run/schedule";
import {
  fmtBytes,
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
import { POPULATION, reasonLabel, STAGE, STAGE_STATE, TIP, type StageState, type Word } from "../../view/words";
import { CARDS, FACT, type FactKey } from "./copy";

/** Sets the card's look, its edge, glow and quiet dash. */
type CardStatus = "pending" | "not-run" | "active" | "recovering" | "stopped" | "complete" | "partial" | "failed";

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

/** A card shows from this, the run's evidence or one server's share of it. */
export interface Evidence {
  /** Settled stages only. */
  status: Partial<Record<Stage, StageStatus>>;
  download: Transfer | null;
  upload: Transfer | null;
  bidirectional: { down: ThroughputResult | null; up: ThroughputResult | null; wire?: WireModel | null } | null;
  latency: LatencyResult | null;
  /** The latency card's spread, replies and timeouts read the idle population. */
  idle: LatencySummary | null;
}

/** Takes one server's share of a multi-server result; nothing is settled for a server that measured nothing. */
export function serverEvidence(details: MultiServerResult, id: string): Evidence {
  const server = details.servers.find((entry) => entry.server.id === id);
  if (!server) return { status: {}, download: null, upload: null, bidirectional: null, latency: null, idle: null };
  return {
    status: server.stages,
    download: server.download,
    upload: server.upload,
    bidirectional: server.bidirectional,
    latency: server.latency,
    idle: server.latencyByStage.latency,
  };
}

/** Chooses what the cards read, the lens's server or every server together. Shown together, the latency card reads
    the server latency is shown for, whole, once it has a result; every server is probed alike, so its headline and
    its spread never come from two servers. */
export function shownEvidence(view: {
  details: MultiServerResult | null;
  lens: string;
  focus: string | null;
  run: Omit<Evidence, "idle">;
  /** The focus's live replies stand in while it has no result. */
  idle: LatencySummary | null;
}): Evidence {
  const { details, lens, focus, run } = view;
  if (details && lens) return serverEvidence(details, lens);
  const focused = details?.servers.find((entry) => entry.server.id === focus);
  if (!focused?.latency) return { ...run, idle: view.idle };
  return {
    ...run,
    status: { ...run.status, latency: focused.stages.latency },
    latency: focused.latency,
    idle: focused.latencyByStage.latency,
  };
}

const SETTLED = new Set<StageStatus>(["complete", "partial", "failed"]);
export const settled = (status: StageStatus | null | undefined): status is "complete" | "partial" | "failed" =>
  !!status && SETTLED.has(status);

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

/** Words a card's state; a stage running its warmup is Running, one a stopped run never reached is Skipped. */
function wordOf(state: StageState): Word | null {
  if (state === "upcoming" || state === "complete") return null;
  return state === "warmup" ? STAGE_STATE.running : state === "unreached" ? STAGE_STATE.skipped : STAGE_STATE[state];
}

const TRANSFER_FACTS: readonly FactKey[] = ["peak", "stability", "transferred"];
const FACTS: Record<Stage, readonly FactKey[]> = {
  latency: ["stability", "range", "replies", "timeouts"],
  download: TRANSFER_FACTS,
  upload: TRANSFER_FACTS,
  bidirectional: ["stability", "combined", "transferred"],
};
type Known = Partial<Record<FactKey, string>>;

/** Fills the stage's fixed facts from `known`. */
function facts(stage: Stage, known: Known): Fact[] {
  return FACTS[stage].map((key) => ({
    key,
    label: FACT[key].label,
    value: known[key] ?? MISSING,
    tip: stage === "latency" && key === "stability" ? TIP.latencyStability : FACT[key].tip,
  }));
}

const percent = (pct: number | null | undefined) => (pct == null ? undefined : fmtPercent(pct));

/** Reads the idle replies' spread, count and unanswered share. */
const idleFacts = (idle: LatencySummary | null): Known => ({
  range: idle ? fmtLatencyRange(idle.minMs, idle.maxMs) : undefined,
  replies: idle ? String(idle.probeCount - idle.timeoutCount) : undefined,
  timeouts: idle?.probeCount ? fmtPercent((100 * idle.timeoutCount) / idle.probeCount) : undefined,
});

/** Time without data shows from half a second. */
const noData = (ms: number | undefined) => (ms !== undefined && ms >= 500 ? fmtDuration(ms) : null);

const STACK = ["application-framing", "tls-records", "transport", "ip", "ethernet"];

/** From half a percent of overhead the wire rate sits under the headline, its layers in its tip. */
function wire(model: WireModel | null | undefined, bytesPerSec: number, units: RateUnits): CardModel["wire"] {
  if (!model || model.totalMultiplier < 1.005) return null;
  const shown = model.factors
    .filter((factor) => factor.contributionPct > 0)
    .sort((a, b) => STACK.indexOf(a.key) - STACK.indexOf(b.key) || b.contributionPct - a.contributionPct);
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
  const figure = (value: number | null) => (value === null ? MISSING : fmtSpeed(toRateUnit(value, units, tier)));
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

/** Builds a settled stage's card, live or saved. */
export function settledCard(
  stage: Stage,
  status: "complete" | "partial" | "failed",
  evidence: Evidence,
  units: RateUnits,
  showWire: boolean,
): CardModel {
  const card = blank(stage, status);
  const complete = status === "complete";
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
  /** This stage's measured bytes so far. */
  bytes: number;
  /** Peak and Stability so far come from the stage's series by the second. */
  shape: { peak: number; stability: number | null } | null;
}

/** Builds the card of a stage still to come, running, stopped, or one the run will never settle. */
export function liveCard(stage: Stage, state: StageState, live: LiveEvidence, units: RateUnits): CardModel {
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
  if (stage !== "bidirectional") known.peak = live.shape ? fmtRate(live.shape.peak, units) : undefined;
  const held = stopped ? reading(live.held) : { figure: MISSING, unit: rateUnit(units, units.tier ?? 0) };
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

/** Finds Peak and Stability so far from each lane's mean per whole second since `start`, lanes added per second. */
export function shapeSoFar(
  samples: readonly { t: number; bytesPerSec: number; stage: string; dir: string }[],
  stage: TransferStage,
  start: number,
): LiveEvidence["shape"] {
  const seconds = new Map<number, Map<string, { sum: number; n: number }>>();
  for (const sample of samples) {
    if (sample.stage !== stage || sample.t < start) continue;
    const second = Math.floor((sample.t - start) / 1000);
    const lanes = seconds.get(second) ?? new Map<string, { sum: number; n: number }>();
    seconds.set(second, lanes);
    const cell = lanes.get(sample.dir) ?? { sum: 0, n: 0 };
    lanes.set(sample.dir, cell);
    cell.sum += sample.bytesPerSec;
    cell.n++;
  }
  if (!seconds.size) return null;
  const rates = [...seconds.entries()]
    .sort(([a], [b]) => a - b)
    .map(([, lanes]) => [...lanes.values()].reduce((total, { sum, n }) => total + sum / n, 0));
  return { peak: Math.max(...rates), stability: rates.length >= 2 ? stabilityPct(rates) : null };
}

export interface Issue {
  server: string;
  stages: string;
  reason: string;
  throughput: Stage[];
}

/** Lists failed server stages, one line per server and reason saying who, which stages and why. A transfer's lost
    probes are named as the lanes name them; only throughput failures mark a card. `scope` narrows to one server. */
export function serverIssues(details: MultiServerResult, scope = ""): Issue[] {
  const lines = new Map<string, { server: string; stages: string[]; reason: string; throughput: Stage[] }>();
  for (const failure of details.failures) {
    if (scope && failure.serverId !== scope) continue;
    const key = `${failure.serverId} ${failure.reason}`;
    let line = lines.get(key);
    if (!line) {
      const server = details.selection.find((entry) => entry.id === failure.serverId)?.name ?? "Server";
      line = { server, stages: [], reason: reasonLabel(failure.reason), throughput: [] };
      lines.set(key, line);
    }
    const latency = failure.scope === "latency";
    line.stages.push(latency && failure.stage !== "latency" ? POPULATION[failure.stage] : STAGE[failure.stage].label);
    if (!latency) line.throughput.push(failure.stage);
  }
  return [...lines.values()].map(({ stages, ...line }) => ({ ...line, stages: stages.join(", ") }));
}

/** Gives a card's failure reason, named by server when several ran and all are shown. */
export function cardReason(issues: readonly Issue[], stage: Stage, attributed: boolean): string {
  return issues
    .filter((issue) => issue.throughput.includes(stage))
    .map(({ server, reason }) =>
      attributed ? `${server}: ${reason.charAt(0).toLowerCase()}${reason.slice(1)}` : reason,
    )
    .join(", ");
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
