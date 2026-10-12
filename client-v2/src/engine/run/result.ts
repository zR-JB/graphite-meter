// What a finished run reports and the rules that judge it: stage statuses, the outcome, and the coherence a saved
// result must hold. History keeps these shapes (schema 5) and older builds read them back, so they stay as they are.
import { FAILURE_REASONS, type FailureReason } from "../../protocol/reasons";
import { directions, sufficient, type Interval, type ThroughputResult } from "../measure/aggregate";
import type { AddedLatency, LatencyResult, LatencySummary } from "../measure/latency";
import type { WireModel } from "../measure/overhead";
import { minCoverage } from "../measure/stability";
import type { HttpProtocol, ServerIdentity } from "../types";
import { STAGES, type Stage, type TransferStage } from "./schedule";

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
  /** At most 256 characters, empty when the server's text carried control characters. */
  message: string;
}

/** A run-level transfer headline with its wire estimate. */
export type StageThroughput = ThroughputResult & { wire: WireModel | null };

export interface StageResults {
  latency: LatencyResult | null;
  download: StageThroughput | null;
  upload: StageThroughput | null;
  /** Each lane without its own estimate; one combined estimate when both lanes have one. */
  bidirectional: { down: ThroughputResult | null; up: ThroughputResult | null; wire: WireModel | null } | null;
}

/** A server's share of the run; its throughput results come from its own full windows. */
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
  /** The run's servers in catalogue order, then those that failed their start check. */
  selection: ServerIdentity[];
  /** Servers still in the run. */
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
  /** Epoch ms. */
  startedAt: number;
  /** Run ms, preparation, finalization and page gaps included. */
  durationMs: number;
}

/** What each stage was measured against when it settled; the settings may have changed since. */
export type StagePlans = Record<Stage, { planned: boolean; durationMs: number; early: boolean }>;

export const failureScope = (stage: Stage): Scope => (stage === "latency" ? "latency" : "throughput");

export const lanes = (
  results: Record<"latency" | "download" | "upload", unknown> & {
    bidirectional: { down: unknown; up: unknown } | null;
  },
  stage: Stage,
): unknown[] =>
  stage === "bidirectional" ? [results.bidirectional?.down, results.bidirectional?.up] : [results[stage]];

/** The run and each server share one rule: every lane, then no failure. */
export const stageStatus = (results: unknown[], failed: boolean): StageStatus =>
  !results.every(Boolean) ? "failed" : failed ? "partial" : "complete";

export const outcomeOf = (statuses: StageStatus[], failures: number): Outcome =>
  statuses.includes("failed") ? "incomplete" : failures ? "partial" : "complete";

/** The bytes every server moved, both directions. */
export const movedBytes = (result: MultiServerResult) =>
  result.servers.reduce((sum, { totalBytes }) => sum + totalBytes.down + totalBytes.up, 0);

/** From the stage's first kept interval to its last, gaps between them included. */
export function coveredMs(intervals: readonly Interval[], stage: TransferStage): number {
  const spans = intervals.filter((interval) => interval.stage === stage);
  return spans.length ? spans.at(-1)!.endMs - spans[0]!.startMs : 0;
}

/** Invariants a saved result holds; the run downgrades one that breaks them to incomplete. */
export function incoherence(result: RunResult, plans: StagePlans): string[] {
  const problems: string[] = [];
  const { failures, intervals, omittedIntervals } = result.multiServer;
  // Past 128 intervals the oldest give way; a stage that ran up to the first kept one cannot be checked against them.
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
