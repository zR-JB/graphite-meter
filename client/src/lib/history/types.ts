import {
  FAILURE_REASONS,
  type AddedLatency,
  type LatencyResult,
  type RunResult,
  type RunnerConfig,
  type StageLatencySummary,
  type StageStatus,
  type ThroughputResult,
  type TransportRole,
} from "../runner/contract";
import type { CompensationBreakdown } from "../compensation";
import { isRecord } from "../api/decode";
import { createUuid } from "../uuid";
import { planned, STAGES } from "../runner/schedule";
import {
  EARLY_FINISH,
  sufficient,
  type LatencyLaneSnapshot,
  type MultiServerResult,
  type TransferStage,
} from "../runner/measure";

export type { StageStatus };

const HISTORY_SCHEMA_VERSION = 5 as const;
export const HISTORY_LIMIT = 2_000 as const;

export interface HistoryRecord {
  schemaVersion: typeof HISTORY_SCHEMA_VERSION;
  id: string;
  completedAt: number;
  build: string;
  engine: string;
  result: RunResult;
}

export function buildHistoryRecord(
  result: RunResult,
  meta: Pick<HistoryRecord, "build" | "engine">,
  config?: Parameters<typeof incoherence>[1],
  completedAt = Date.now(),
): HistoryRecord {
  const record: HistoryRecord = {
    schemaVersion: HISTORY_SCHEMA_VERSION,
    id: createUuid(),
    completedAt: Math.trunc(completedAt),
    ...meta,
    result: structuredClone(result),
  };
  const problems = incoherence(record.result, config);
  if (problems.length && record.result.outcome !== "incomplete") {
    console.error("Incoherent result saved as incomplete:", problems);
    record.result.outcome = "incomplete";
  }
  return record;
}

const lanesOf = (result: RunResult, stage: TransportRole) =>
  stage === "bidirectional"
    ? [result.bidirectional?.down, result.bidirectional?.up]
    : [result[stage]];

/** Invariants a saved result must hold; with the run's config, planned stages must also be covered. */
export function incoherence(
  result: RunResult,
  config?: Pick<RunnerConfig, "stages" | "duration" | "adaptive">,
): string[] {
  const problems: string[] = [];
  const { failures, intervals } = result.multiServer;
  for (const failure of failures)
    if (!FAILURE_REASONS.includes(failure.reason))
      problems.push(`unknown failure reason ${failure.reason}`);
  for (const name of STAGES) {
    const status = result.stages[name];
    const lanes = lanesOf(result, name);
    const scope = name === "latency" ? "latency" : "throughput";
    const explained = failures.some(
      (f) => f.stage === name && f.scope === scope,
    );
    const spans = intervals.filter((i) => i.stage === name);
    const wanted = !!config && planned(config, name);
    if (config && wanted === (status === "not-run"))
      problems.push(`${name} is ${status} but planned ${wanted}`);
    if ((status === "failed" || status === "partial") && !explained)
      problems.push(`${name} is ${status} without a stated reason`);
    if (status === "complete" && (explained || !lanes.every(Boolean)))
      problems.push(
        `${name} is complete without every result or with a failure`,
      );
    if (status === "not-run" && (lanes.some(Boolean) || spans.length))
      problems.push(`${name} is not-run but has evidence`);
    if (name !== "latency" && status === "complete") {
      const dirs =
        name === "bidirectional"
          ? (["down", "up"] as const)
          : ([name === "download" ? "down" : "up"] as const);
      if (
        !dirs.every((dir) =>
          spans.some(({ headline }) => sufficient(headline, dir)),
        )
      )
        problems.push(`${name} is complete without 800 ms of evidence`);
      const plannedMs = config?.duration[`${name}Ms`] ?? 0;
      // From the first to the last evidence, so a hidden-page gap between intervals still counts as run time.
      const covered = spans.length ? spans.at(-1)!.endMs - spans[0].startMs : 0;
      const floor = config?.adaptive ? EARLY_FINISH.minCoverage : 0.75;
      if (config && covered < plannedMs * floor)
        problems.push(
          `${name} covers ${Math.round(covered)} of ${plannedMs} ms`,
        );
    }
  }
  const statuses = Object.values(result.stages);
  if (statuses.every((status) => status === "not-run"))
    problems.push("no stage ran");
  const expected = statuses.includes("failed")
    ? "incomplete"
    : failures.length
      ? "partial"
      : "complete";
  if (result.outcome !== expected)
    problems.push(`outcome ${result.outcome} should be ${expected}`);
  return problems;
}

const time = (value: unknown) =>
  typeof value === "number" && !Number.isNaN(new Date(value).getTime());

/** Saved leaves are finite numbers, bounded text, booleans, null or omitted. */
function plain(value: unknown, depth = 0): boolean {
  if (value == null || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (typeof value === "string") return value.length <= 2048;
  if (typeof value !== "object" || depth > 8) return false;
  const entries = Array.isArray(value) ? value : Object.values(value);
  return (
    entries.length <= 512 && entries.every((entry) => plain(entry, depth + 1))
  );
}

type Check = (value: unknown) => boolean;
const num: Check = (value) => Number.isFinite(value);
const str: Check = (value) => typeof value === "string";
const maybe =
  (check: Check): Check =>
  (value) =>
    value == null || check(value);
const list =
  (check: Check): Check =>
  (value) =>
    Array.isArray(value) && value.every(check);
const shape =
  (fields: Record<string, Check>): Check =>
  (value) =>
    isRecord(value) &&
    Object.entries(fields).every(([key, check]) => check(value[key]));
const each = (check: Check) =>
  shape(Object.fromEntries(STAGES.map((stage) => [stage, check])));

const rate = maybe(
  shape({
    reportedBytesPerSec: num,
    totalBytes: num,
    stabilityPct: num,
    wire: maybe(
      shape({
        totalMultiplier: num,
        mtuBytes: num,
        factors: list(shape({ label: str, contributionPct: num })),
      }),
    ),
  }),
);
const lanes = maybe(shape({ down: rate, up: rate }));
const populations = each(maybe(shape({ p50Ms: maybe(num), probeCount: num })));
const measured = {
  addedLatency: maybe(
    shape({
      download: maybe(num),
      upload: maybe(num),
      bidirectional: maybe(num),
    }),
  ),
  latency: maybe(shape({ reportedMs: num })),
  latencyByStage: populations,
  download: rate,
  upload: rate,
  bidirectional: lanes,
};

/** What History's rows, sort and detail dereference; a leaf of another type meets the views' boundaries. */
const readable = shape({
  schemaVersion: (value) => value === HISTORY_SCHEMA_VERSION,
  id: str,
  completedAt: time,
  build: str,
  engine: str,
  result: shape({
    ...measured,
    outcome: (value) =>
      ["complete", "partial", "incomplete"].includes(`${value}`),
    stages: isRecord,
    durationMs: num,
    multiServer: shape({
      latencyFocus: str,
      selection: list(shape({ id: str, name: str, url: str })),
      servers: list(
        shape({
          ...measured,
          server: shape({ id: str }),
          throughput: shape({ transport: str }),
          latencyTarget: maybe(shape({ transport: str })),
          totalBytes: shape({ down: num, up: num }),
          stages: isRecord,
        }),
      ),
      failures: list(
        shape({
          serverId: str,
          stage: (value) => STAGES.some((stage) => stage === value),
          reason: str,
        }),
      ),
    }),
  }),
});

/** A saved record, with schema 4 lifted into a result, or null when a view could not read it. */
export function readHistoryRecord(value: unknown): HistoryRecord | null {
  if (!isRecord(value) || !plain(value)) return null;
  const record =
    value.schemaVersion === 4 && liftable(value) ? fromSchema4(value) : value;
  return readable(record) ? (record as HistoryRecord) : null;
}

/** Schema 4 as main saved it: flat per-stage fields for one server, or a result per server. */
interface Schema4 {
  id: string;
  completedAt: number;
  startedAt: number;
  durationMs: number;
  outcome?: RunResult["outcome"];
  multiServer?: MultiServerResult;
  stages: {
    latency: {
      status: StageStatus;
      result: LatencyResult | null;
      lanes: Record<TransportRole, LatencyLaneSnapshot | null>;
    };
    download: { status: StageStatus; result: ThroughputResult | null };
    upload: { status: StageStatus; result: ThroughputResult | null };
    bidirectional: {
      status: StageStatus;
      down: ThroughputResult | null;
      up: ThroughputResult | null;
    };
  };
  bufferbloat: { addedMs?: Partial<AddedLatency> } | null;
  totalBytes: number;
  server: { name: string; location: string | null; engine: string };
  transport: Record<
    "throughput" | "latency",
    { protocol: string | null; kind: string | null }
  >;
  ipVersion: 4 | 6 | null;
  client: { build: string };
  wireEstimates: {
    breakdown: Record<TransferStage, CompensationBreakdown | null>;
    downloadBytesPerSec: number | null;
    uploadBytesPerSec: number | null;
    bidirectionalBytesPerSec: number | null;
  } | null;
}

const liftable = shape({
  id: str,
  stages: shape({
    latency: shape({ lanes: each(maybe(isRecord)) }),
    download: isRecord,
    upload: isRecord,
    bidirectional: isRecord,
  }),
  server: isRecord,
  client: isRecord,
  transport: shape({ throughput: isRecord, latency: isRecord }),
  wireEstimates: maybe(shape({ breakdown: isRecord })),
  multiServer: maybe(shape({ servers: list(isRecord) })),
}) as (value: unknown) => value is Schema4;

const summary = (
  lane: LatencyLaneSnapshot | null,
): StageLatencySummary | null =>
  lane && {
    ...(lane.reflectorTiming ? { reflectorTiming: lane.reflectorTiming } : {}),
    accountingComplete: lane.accountingComplete,
    probeCount: lane.count,
    timeoutCount: lane.timeoutCount,
    unresolvedCount: lane.unresolvedCount,
    sendFailureCount: lane.sendFailureCount,
    jitterPairs: 0,
    minMs: lane.min,
    maxMs: lane.max,
    meanMs: null,
    p10Ms: lane.p10,
    p50Ms: lane.center,
    p90Ms: lane.p90,
    p95Ms: lane.p95 ?? null,
    jitterMs: lane.jitter,
  };

function fromSchema4(saved: Schema4): HistoryRecord {
  const { stages, wireEstimates } = saved;
  const wire = (stage: TransferStage, measured: number) => {
    const breakdown = wireEstimates?.breakdown[stage];
    const estimated = wireEstimates?.[`${stage}BytesPerSec`];
    return breakdown && estimated && measured
      ? { ...breakdown, totalMultiplier: estimated / measured }
      : null;
  };
  const lane = (stage: "download" | "upload") => {
    const result = stages[stage].result;
    return (
      result && { ...result, wire: wire(stage, result.reportedBytesPerSec) }
    );
  };
  const { down, up, status } = stages.bidirectional;
  const results = {
    latency: stages.latency.result,
    download: lane("download"),
    upload: lane("upload"),
    bidirectional:
      status === "not-run"
        ? null
        : {
            down,
            up,
            wire:
              down && up
                ? wire(
                    "bidirectional",
                    down.reportedBytesPerSec + up.reportedBytesPerSec,
                  )
                : null,
          },
    latencyByStage: Object.fromEntries(
      STAGES.map((stage) => [stage, summary(stages.latency.lanes[stage])]),
    ) as RunResult["latencyByStage"],
    addedLatency: saved.bufferbloat?.addedMs
      ? {
          download: null,
          upload: null,
          bidirectional: null,
          ...saved.bufferbloat.addedMs,
        }
      : null,
  };
  const statuses = Object.fromEntries(
    STAGES.map((stage) => [stage, stages[stage].status]),
  ) as RunResult["stages"];
  const server = {
    id: "self",
    url: "",
    name: saved.server.name,
    ...(saved.server.location ? { location: saved.server.location } : {}),
  };
  const { throughput, latency } = saved.transport;
  const multiServer: MultiServerResult = saved.multiServer
    ? {
        ...saved.multiServer,
        servers: saved.multiServer.servers.map((entry) => ({
          ...entry,
          stages: statuses,
        })),
      }
    : {
        selection: [server],
        participants: [server.id],
        latencyFocus: server.id,
        intervals: [],
        omittedIntervals: 0,
        failures: [],
        servers: [
          {
            server,
            throughput: {
              origin: "",
              transport: throughput.kind ?? "",
              protocol: throughput.protocol ?? "",
              ...(saved.ipVersion ? { clientIpVersion: saved.ipVersion } : {}),
            },
            latencyTarget: latency.kind
              ? { origin: "", transport: latency.kind }
              : null,
            ...results,
            totalBytes: { down: saved.totalBytes, up: 0 },
            stages: statuses,
          },
        ],
      };
  return {
    schemaVersion: HISTORY_SCHEMA_VERSION,
    id: saved.id,
    completedAt: saved.completedAt,
    build: saved.client.build,
    engine: saved.server.engine,
    result: {
      ...results,
      multiServer,
      stages: statuses,
      outcome:
        saved.outcome ??
        (Object.values(statuses).includes("failed")
          ? "incomplete"
          : Object.values(statuses).includes("partial")
            ? "partial"
            : "complete"),
      startedAt: saved.startedAt,
      durationMs: saved.durationMs,
    },
  };
}
