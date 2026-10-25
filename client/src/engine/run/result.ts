// What a finished run reports and the rules that judge it; History keeps these shapes as schema 6.
import { FAILURE_REASONS, type FailureReason } from "../../protocol/reasons";
import {
  coveredMs,
  directions,
  MIN_EVIDENCE_MS,
  sufficient,
  type Interval,
  type ThroughputResult,
} from "../measure/aggregate";
import type { AddedLatency, LatencyResult, LatencySummary } from "../measure/latency";
import type { WireModel } from "../measure/overhead";
import { minCoverage } from "../measure/stability";
import type { HttpProtocol, Role, ServerIdentity } from "../types";
import { STAGES, type Stage, type TransferStage } from "./schedule";

export type StageStatus = "complete" | "partial" | "failed" | "not-run";
export type Outcome = "complete" | "partial" | "incomplete";

export interface ServerFailure {
  serverId: string;
  stage: Stage;
  atMs: number;
  scope: Role;
  reason: FailureReason;
  /** What happened, as a sentence that names the server and the stage; never the reason's label again. */
  message: string;
  /** The engine's or the server's own text, for a report: at most 256 characters, empty when it carried control
   *  characters. Records before 0.14 kept it as `message`. */
  detail?: string;
}

type StageThroughput = ThroughputResult & { wire: WireModel | null };

export interface StageResults {
  latency: LatencyResult | null;
  download: StageThroughput | null;
  upload: StageThroughput | null;
  /** Each lane without its own estimate; one combined estimate when both lanes have one. */
  bidirectional: {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
    wire: WireModel | null;
  } | null;
}

/** A server's share of the run; its throughput results come from its own full windows. */
export interface ServerMeasurementSummary {
  server: ServerIdentity;
  engineVersion?: string;
  throughput: {
    origin: string;
    transport: string;
    protocol: HttpProtocol;
    browserProtocol?: string;
    clientIpVersion?: 4 | 6;
  };
  latencyTarget: { origin: string; transport: string } | null;
  latency: LatencyResult | null;
  latencyByStage: Record<Stage, LatencySummary | null>;
  addedLatency: AddedLatency | null;
  download: Omit<ThroughputResult, "quietMs"> | null;
  upload: Omit<ThroughputResult, "quietMs"> | null;
  /** Null unless the run entered the bidirectional stage. */
  bidirectional: {
    down: Omit<ThroughputResult, "quietMs"> | null;
    up: Omit<ThroughputResult, "quietMs"> | null;
  } | null;
  /** Unique measured bytes over every stage. */
  totalBytes: { down: number; up: number };
  stages: Record<Stage, StageStatus>;
}

export interface MultiServerResult {
  /** The run's servers in catalog order, then those that failed their start check. */
  selection: ServerIdentity[];
  participants: string[];
  latencyFocus: string;
  servers: ServerMeasurementSummary[];
  intervals: Interval[];
  omittedIntervals: number;
  failures: ServerFailure[];
}

export interface RunResult extends StageResults {
  multiServer: MultiServerResult;
  outcome: Outcome;
  stages: Record<Stage, StageStatus>;
  /** The latency focus server's populations. */
  latencyByStage: Record<Stage, LatencySummary | null>;
  addedLatency: AddedLatency | null;
  startedAt: number;
  /** Run ms, preparation, finalization and page gaps included. */
  durationMs: number;
}

/** What each stage was measured against when it settled; the settings may have changed since. */
export type StagePlans = Record<Stage, { planned: boolean; durationMs: number; early: boolean }>;

export const failureScope = (stage: Stage): Role =>
  stage === "latency" ? "latency" : "throughput";

const TRANSFER: Record<TransferStage, (server: string) => string> = {
  download: (server) => `Download from ${server}`,
  upload: (server) => `Upload to ${server}`,
  bidirectional: (server) => `The bidirectional transfer with ${server}`,
};
const DURING: Record<Stage, string> = {
  latency: "during the latency stage",
  download: "during the download",
  upload: "during the upload",
  bidirectional: "during the bidirectional transfer",
};
/** What a server short of evidence did too little of. */
const SHORT: Record<TransferStage, string> = {
  download: "sent too little download data",
  upload: "received too little upload data",
  bidirectional: "exchanged too little data in both directions",
};

/** A transfer stage whose evidence spans too little of its planned time, on one of its servers. */
export const shortSentence = (stage: TransferStage, server: string) =>
  `${TRANSFER[stage](server)} covered less of its planned time than a result needs.`;

/** A failure as a reader of the result says it: what happened, to which server, in which stage or at Start. */
export function failureSentence(
  { reason, stage, scope }: Pick<Cause, "reason" | "stage" | "scope">,
  server: string,
): string {
  if (stage === "start")
    switch (reason) {
      case "server-capacity":
        return `${server} had no room for another test when this one started.`;
      case "client-limit":
        return `${server} already runs a test for this address or sign-in.`;
      case "sign-in-required":
        return `${server} admits only signed-in tests.`;
      case "protocol-error":
        return `${server} answered its check at Start in a way this page cannot read.`;
      default:
        return `${server} did not answer its check at Start.`;
    }
  const probes = scope === "latency";
  const transfer = stage === "latency" ? null : stage;
  const loaded = transfer && probes ? ` ${DURING[transfer]}` : "";
  switch (reason) {
    case "connection-lost":
      return !transfer || probes
        ? `Latency probes to ${server} lost their connection${loaded}.`
        : `${TRANSFER[transfer](server)} lost its connection.`;
    case "timeout":
      return !transfer || probes
        ? `${server} stopped answering latency probes${loaded}.`
        : transfer === "bidirectional"
          ? `${server} stopped moving data in both directions.`
          : `${server} stopped delivering ${transfer} data.`;
    case "path-failed":
      return !transfer || probes
        ? `Latency probes could not reach ${server}${loaded}.`
        : `${TRANSFER[transfer](server)} could not start.`;
    case "sign-in-required":
      return `The sign-in to ${server} ended ${DURING[stage]}.`;
    case "client-limit":
      return `${server} refused more connections for this address or sign-in ${DURING[stage]}.`;
    case "server-capacity":
      return `${server} had no room for more connections ${DURING[stage]}.`;
    case "protocol-error":
      return `${server} sent a reply this page cannot read ${DURING[stage]}.`;
    case "insufficient-evidence":
      return !transfer || probes
        ? `${server} answered too few latency probes for a result${loaded}.`
        : `${server} ${SHORT[transfer]} for a result.`;
  }
}

export const lanes = (
  results: Record<"latency" | "download" | "upload", unknown> & {
    bidirectional: { down: unknown; up: unknown } | null;
  },
  stage: Stage,
): unknown[] =>
  stage === "bidirectional"
    ? [results.bidirectional?.down, results.bidirectional?.up]
    : [results[stage]];

/** The run and each server share one rule: every lane, then no failure. */
export const stageStatus = (results: unknown[], failed: boolean): StageStatus =>
  !results.every(Boolean) ? "failed" : failed ? "partial" : "complete";

/** A selected server's stage, or a whole stage: `unreached` took no part, `skipped` was off for the run. */
export type CellStatus = "complete" | "partial" | "failed" | "unreached" | "skipped";
/** Why a cell is not complete; a server dropped at Start left at "start". */
export type Cause = Omit<ServerFailure, "stage"> & { stage: Stage | "start" };
export interface Cell {
  status: CellStatus;
  /** Its own failure, or for an unreached cell the one that took its server out. */
  cause: Cause | null;
}
export interface Row {
  server: ServerIdentity;
  cells: Record<Stage, Cell>;
  /** The server's own outcome by the run's rule: Incomplete when a stage it was planned for has no result of its. */
  outcome: Outcome;
  /** Each of its failures, in stage order; loaded latency's included. */
  causes: Cause[];
}
export interface Grid {
  /** Every selected server, in selection order. */
  rows: Row[];
  /** Complete only when every selected server measured the stage. */
  columns: Record<Stage, CellStatus>;
  outcome: Outcome;
  /** Some stage has a result; a run without one failed, though History keeps it as Incomplete. */
  measured: boolean;
  /** Every server's causes, in selection order. */
  causes: Cause[];
}
type Graded = StageResults & Pick<RunResult, "stages" | "multiServer"> & { outcome?: Outcome };

const order = (stage: Stage | "start") => (stage === "start" ? -1 : STAGES.indexOf(stage));

const perStage = <T>(value: (stage: Stage) => T) =>
  Object.fromEntries(STAGES.map((stage) => [stage, value(stage)])) as Record<Stage, T>;

/** The result as servers by stages, from what schema 6 saves; every surface reads its words from this. A `running`
 *  stage reads each server as it stands so far: failed there, gone before it, or still measuring as complete. */
export function grid(result: Graded, running: Stage | null = null): Grid {
  const { selection, servers, failures, participants } = result.multiServer;
  const rows = selection.map((server) => {
    const summary = servers.find((entry) => entry.server.id === server.id);
    const own = failures.filter((failure) => failure.serverId === server.id);
    // A server outside the run failed its start check; its failure was charged to a stage it never reached.
    const start: Cause | null = summary || !own[0] ? null : { ...own[0], stage: "start" };
    const cell = (stage: Stage): Cell => {
      if (result.stages[stage] === "not-run" && stage !== running)
        return { status: "skipped", cause: null };
      if (!summary) return { status: "unreached", cause: start };
      const failure = own.find((f) => f.stage === stage && f.scope === failureScope(stage));
      // A server without a latency path takes no part in the latency stage by plan, unless charged for it.
      if (stage === "latency" && summary.latencyTarget === null && !failure)
        return { status: "skipped", cause: null };
      const before = STAGES.indexOf(stage);
      const left = own.findLast(
        (f) => f.scope === failureScope(f.stage) && STAGES.indexOf(f.stage) < before,
      );
      // A sole server rejoins at each stage, though no event has said so yet.
      if (stage === running)
        return failure
          ? { status: "failed", cause: failure }
          : participants.includes(server.id) || servers.length === 1
            ? { status: "complete", cause: null }
            : { status: "unreached", cause: left ?? null };
      const status = summary.stages[stage];
      // A server's own lanes decide: a schema 4 record copied the run's statuses onto every server.
      if (status !== "not-run")
        return {
          status: lanes(summary, stage).every(Boolean) ? status : "failed",
          cause: failure ?? null,
        };
      // Charged for a stage it never ran, as a focus without a latency path is: it failed there.
      if (failure) return { status: "failed", cause: failure };
      return { status: "unreached", cause: left ?? null };
    };
    const cells = perStage(cell);
    const statuses = Object.values(cells).map(({ status }) => status);
    // Loaded latency lost because its server left the stage is that leaving, not a cause of its own.
    const consequence = (f: ServerFailure) =>
      f.scope !== failureScope(f.stage) &&
      own.some((g) => g.stage === f.stage && g.scope === failureScope(g.stage));
    const causes: Cause[] = start
      ? [start]
      : own.filter((f) => !consequence(f)).toSorted((a, b) => order(a.stage) - order(b.stage));
    const outcome: Outcome = statuses.some(
      (status) => status === "failed" || status === "unreached",
    )
      ? "incomplete"
      : causes.length || statuses.includes("partial")
        ? "partial"
        : "complete";
    return { server, cells, outcome, causes };
  });
  const columns = perStage((stage): CellStatus => {
    const saved = result.stages[stage];
    if (saved === "not-run") return "skipped";
    const cells = rows.map(({ cells }) => cells[stage].status).filter((s) => s !== "skipped");
    if (!lanes(result, stage).every(Boolean))
      return cells.every((status) => status === "unreached") ? "unreached" : "failed";
    return saved === "complete" && cells.every((status) => status === "complete")
      ? "complete"
      : "partial";
  });
  const statuses = Object.values(columns);
  const outcome: Outcome = statuses.some((status) => status === "failed" || status === "unreached")
    ? "incomplete"
    : failures.length || statuses.includes("partial")
      ? "partial"
      : "complete";
  return {
    rows,
    columns,
    outcome,
    measured: statuses.some((status) => status === "complete" || status === "partial"),
    causes: rows.flatMap((row) => row.causes),
  };
}

/** A column as schema 6 saves it, in the words every release reads. */
const SAVED: Record<CellStatus, StageStatus> = {
  complete: "complete",
  partial: "partial",
  failed: "failed",
  unreached: "failed",
  skipped: "not-run",
};

const OUTCOMES: readonly Outcome[] = ["complete", "partial", "incomplete"];

/** The result with the grid's stages and outcome: what a run saves, and what History reads of any record. */
export function graded<T extends Graded>(result: T): T & { outcome: Outcome } {
  const { columns, outcome } = grid(result);
  // Judging again only adds causes; a saved worse outcome had evidence a record no longer keeps.
  const saved = OUTCOMES.indexOf(result.outcome ?? "complete");
  const { selection, servers, failures } = result.multiServer;
  // Before 0.14 a failure kept its lane's or server's text as its message; it reads as a sentence now.
  const said = failures.map((failure) => {
    if (failure.detail !== undefined) return failure;
    const ran = servers.some(({ server }) => server.id === failure.serverId);
    const name = selection.find(({ id }) => id === failure.serverId)?.name ?? failure.serverId;
    const where = { ...failure, stage: ran ? failure.stage : ("start" as const) };
    return { ...failure, detail: failure.message, message: failureSentence(where, name) };
  });
  return {
    ...result,
    stages: perStage((stage) => SAVED[columns[stage]]),
    outcome: OUTCOMES[Math.max(saved, OUTCOMES.indexOf(outcome))]!,
    multiServer: { ...result.multiServer, failures: said },
  };
}

export const movedBytes = (result: MultiServerResult) =>
  result.servers.reduce((sum, { totalBytes }) => sum + totalBytes.down + totalBytes.up, 0);

/** Invariants a saved result holds; the run downgrades one that breaks them to incomplete. */
export function incoherence(result: RunResult, plans: StagePlans): string[] {
  const problems: string[] = [];
  const { failures, intervals, omittedIntervals } = result.multiServer;
  const { rows, columns, outcome } = grid(result);
  // Past the interval limit the oldest give way; a stage that ran up to the first kept one cannot be checked against them.
  const firstKept = intervals.length ? STAGES.indexOf(intervals[0]!.stage) : STAGES.length;
  for (const failure of failures)
    if (!Object.hasOwn(FAILURE_REASONS, failure.reason))
      problems.push(`unknown failure reason ${failure.reason}`);
  for (const stage of STAGES) {
    const status = result.stages[stage];
    const cells = rows.map((row) => row.cells[stage]);
    // A server that left earlier, or at Start, explains a stage it never reached.
    const explained = cells.some(({ cause }) => cause);
    // What its servers measured is whole when none of them failed in it, however many never reached it.
    const whole =
      lanes(result, stage).every(Boolean) &&
      cells.every((cell) => cell.status !== "partial" && cell.status !== "failed");
    for (const { status: cell, cause } of cells)
      if (!cause && cell !== "complete" && cell !== "skipped")
        problems.push(`${stage} is ${cell} for a server without a stated reason`);
    const spans = intervals.filter((interval) => interval.stage === stage);
    const { planned, durationMs, early } = plans[stage];
    if (planned === (status === "not-run"))
      problems.push(`${stage} is ${status} but planned ${planned}`);
    if ((status === "failed" || status === "partial") && !explained)
      problems.push(`${stage} is ${status} without a stated reason`);
    if (status === "complete" && (explained || columns[stage] !== "complete"))
      problems.push(`${stage} is complete without every server's result or with a failure`);
    if (status === "not-run" && (lanes(result, stage).some(Boolean) || spans.length))
      problems.push(`${stage} is not-run but has evidence`);
    if (
      stage === "latency" ||
      !whole ||
      status === "not-run" ||
      (omittedIntervals > 0 && STAGES.indexOf(stage) <= firstKept)
    )
      continue;
    if (!directions(stage).every((dir) => spans.some(({ headline }) => sufficient(headline, dir))))
      problems.push(`${stage} is ${status} without ${MIN_EVIDENCE_MS} ms of evidence`);
    const covered = coveredMs(intervals, stage);
    if (covered < durationMs * minCoverage(early))
      problems.push(`${stage} covers ${Math.round(covered)} of ${durationMs} ms`);
  }
  if (Object.values(result.stages).every((status) => status === "not-run"))
    problems.push("no stage ran");
  if (result.outcome !== outcome) problems.push(`outcome ${result.outcome} should be ${outcome}`);
  return problems;
}
