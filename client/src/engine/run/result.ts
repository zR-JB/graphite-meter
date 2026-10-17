// Defines what a finished run reports and the rules that judge it, from stage statuses and the outcome to the
// coherence a saved result must hold. History keeps these shapes as its schema 5.
import { FAILURE_REASONS, type FailureReason } from "../../protocol/reasons";
import { coveredMs, directions, sufficient, type Interval, type ThroughputResult } from "../measure/aggregate";
import type { AddedLatency, LatencyResult, LatencySummary } from "../measure/latency";
import type { WireModel } from "../measure/overhead";
import { minCoverage } from "../measure/stability";
import type { HttpProtocol, ServerIdentity } from "../types";
import { STAGES, type Stage } from "./schedule";

export type StageStatus = "complete" | "partial" | "failed" | "not-run";
export type Outcome = "complete" | "partial" | "incomplete";
export type Scope = "throughput" | "latency";

export interface ServerFailure {
  serverId: string;
  stage: Stage;
  /** Run ms. */
  atMs: number;
  scope: Scope;
  reason: FailureReason;
  /** Holds at most 256 characters, and is empty when the server's text carried control characters. */
  message: string;
}

/** Pairs a run-level transfer headline with its wire estimate. */
type StageThroughput = ThroughputResult & { wire: WireModel | null };

export interface StageResults {
  latency: LatencyResult | null;
  download: StageThroughput | null;
  upload: StageThroughput | null;
  /** Carries each lane without its own estimate, and one combined estimate when both lanes have one. */
  bidirectional: { down: ThroughputResult | null; up: ThroughputResult | null; wire: WireModel | null } | null;
}

/** Holds a server's share of the run. Its throughput results come from its own full windows. */
export interface ServerMeasurementSummary {
  server: ServerIdentity;
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
  /** Stays null unless the run entered the bidirectional stage. */
  bidirectional: {
    down: Omit<ThroughputResult, "quietMs"> | null;
    up: Omit<ThroughputResult, "quietMs"> | null;
  } | null;
  /** Sums unique measured bytes over every stage. */
  totalBytes: { down: number; up: number };
  stages: Record<Stage, StageStatus>;
}

export interface MultiServerResult {
  /** Lists the run's servers in catalog order, then those that failed their start check. */
  selection: ServerIdentity[];
  /** Lists the servers still in the run. */
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
  /** Holds the latency focus server's populations. */
  latencyByStage: Record<Stage, LatencySummary | null>;
  addedLatency: AddedLatency | null;
  /** Epoch ms. */
  startedAt: number;
  /** Run ms, preparation, finalization and page gaps included. */
  durationMs: number;
}

/** Records what each stage was measured against when it settled. The settings may have changed since. */
export type StagePlans = Record<Stage, { planned: boolean; durationMs: number; early: boolean }>;

export const failureScope = (stage: Stage): Scope => (stage === "latency" ? "latency" : "throughput");

export const lanes = (
  results: Record<"latency" | "download" | "upload", unknown> & {
    bidirectional: { down: unknown; up: unknown } | null;
  },
  stage: Stage,
): unknown[] =>
  stage === "bidirectional" ? [results.bidirectional?.down, results.bidirectional?.up] : [results[stage]];

/** The run and each server share one rule, which checks every lane first and then any failure. */
export const stageStatus = (results: unknown[], failed: boolean): StageStatus =>
  !results.every(Boolean) ? "failed" : failed ? "partial" : "complete";

export const outcomeOf = (statuses: StageStatus[], failures: number): Outcome =>
  statuses.includes("failed") ? "incomplete" : failures ? "partial" : "complete";

/** Sums the bytes every server moved, both directions. */
export const movedBytes = (result: MultiServerResult) =>
  result.servers.reduce((sum, { totalBytes }) => sum + totalBytes.down + totalBytes.up, 0);

/** Lists the invariants a saved result holds. The run downgrades one that breaks them to incomplete. */
export function incoherence(result: RunResult, plans: StagePlans): string[] {
  const problems: string[] = [];
  const { failures, intervals, omittedIntervals } = result.multiServer;
  // Past 128 intervals the oldest give way. A stage that ran up to the first kept one cannot be checked against them.
  const firstKept = intervals.length ? STAGES.indexOf(intervals[0]!.stage) : STAGES.length;
  for (const failure of failures)
    if (!Object.hasOwn(FAILURE_REASONS, failure.reason)) problems.push(`unknown failure reason ${failure.reason}`);
  for (const stage of STAGES) {
    const status = result.stages[stage];
    const explained = failures.some((f) => f.stage === stage && f.scope === failureScope(stage));
    const spans = intervals.filter((interval) => interval.stage === stage);
    const { planned, durationMs, early } = plans[stage];
    if (planned === (status === "not-run")) problems.push(`${stage} is ${status} but planned ${planned}`);
    if ((status === "failed" || status === "partial") && !explained)
      problems.push(`${stage} is ${status} without a stated reason`);
    if (status === "complete" && (explained || !lanes(result, stage).every(Boolean)))
      problems.push(`${stage} is complete without every result or with a failure`);
    if (status === "not-run" && (lanes(result, stage).some(Boolean) || spans.length))
      problems.push(`${stage} is not-run but has evidence`);
    if (stage === "latency" || status !== "complete" || (omittedIntervals > 0 && STAGES.indexOf(stage) <= firstKept))
      continue;
    if (!directions(stage).every((dir) => spans.some(({ headline }) => sufficient(headline, dir))))
      problems.push(`${stage} is complete without 800 ms of evidence`);
    const covered = coveredMs(intervals, stage);
    if (covered < durationMs * minCoverage(early))
      problems.push(`${stage} covers ${Math.round(covered)} of ${durationMs} ms`);
  }
  const statuses = Object.values(result.stages);
  if (statuses.every((status) => status === "not-run")) problems.push("no stage ran");
  const expected = outcomeOf(statuses, failures.length);
  if (result.outcome !== expected) problems.push(`outcome ${result.outcome} should be ${expected}`);
  return problems;
}
