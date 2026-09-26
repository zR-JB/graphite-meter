import type { WireEstimates } from "./wire";
import type {
  PreparedPaths,
  RunResult,
  ThroughputResult,
  LatencyResult,
  TerminationReason,
  TransportKind,
} from "../runner/contract";
import { createUuid } from "../uuid";
import {
  latencyLanes,
  type LatencyLaneSnapshot,
  type MultiServerResult,
  type ServerFailure,
} from "../runner/measure";

const HISTORY_SCHEMA_VERSION = 4 as const;
export const HISTORY_LIMIT = 2_000 as const;
const HISTORY_FAILURE_STAGES = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const;
const MAX_HISTORY_TEXT_LENGTH = 256;

export type StageStatus = "complete" | "partial" | "failed" | "not-run";
interface FailureSnapshot {
  stage: "latency" | "download" | "upload" | "bidirectional";
  direction: "down" | "up" | null;
  reason: Exclude<TerminationReason, "user-abort">;
}
export interface ThroughputSnapshot {
  reportedBytesPerSec: number;
  peakBytesPerSec: number;
  fullAverageBytesPerSec: number;
  method: "stable-window" | "full-average";
  totalBytes: number;
  stabilityPct: number;
  stabilityScore: number;
  band: "low" | "medium" | "high";
  serverAuthoritative: boolean;
}
interface LatencySnapshot {
  reportedMs: number;
  minMs: number | null;
  p50Ms: number | null;
  p95Ms: number | null;
  jitterMs: number | null;
  probeTimeoutPct: number | null;
  method: "stable-window" | "full-average";
  stabilityScore: number;
  band: "low" | "medium" | "high";
}
type ThroughputTransportKind = Extract<
  TransportKind,
  "fetch-stream" | "webtransport" | "webtransport-datagram"
>;
type LatencyTransportKind = Extract<
  TransportKind,
  "websocket" | "webtransport"
>;
export interface HistoryRecord {
  schemaVersion: typeof HISTORY_SCHEMA_VERSION;
  multiServer?: MultiServerResult;
  outcome?: RunResult["outcome"];
  id: string;
  startedAt: number;
  completedAt: number;
  durationMs: number;
  stages: {
    latency: {
      status: StageStatus;
      result: LatencySnapshot | null;
      lanes: Record<
        "latency" | "download" | "upload" | "bidirectional",
        LatencyLaneSnapshot | null
      >;
    };
    download: { status: StageStatus; result: ThroughputSnapshot | null };
    upload: { status: StageStatus; result: ThroughputSnapshot | null };
    bidirectional: {
      status: StageStatus;
      down: ThroughputSnapshot | null;
      up: ThroughputSnapshot | null;
    };
  };
  bufferbloat: {
    /** Signed per-stage added latency; records saved before it lack the field. */
    addedMs?: Partial<
      Record<"download" | "upload" | "bidirectional", number | null>
    >;
    idleMs: number;
    loadedMs: number;
    increaseMs: number;
    grade: string;
  } | null;
  totalBytes: number;
  server: { name: string; location: string | null; engine: string };
  transport: {
    throughput: {
      protocol: string | null;
      kind: ThroughputTransportKind | null;
    };
    latency: { protocol: string | null; kind: LatencyTransportKind | null };
  };
  ipVersion: 4 | 6 | null;
  client: { build: string };
  failures: FailureSnapshot[];
  wireEstimates: WireEstimates | null;
}

function throughputTransportKind(
  value: TransportKind | undefined,
): ThroughputTransportKind | null {
  return value === "fetch-stream" ||
    value === "webtransport" ||
    value === "webtransport-datagram"
    ? value
    : null;
}

function latencyTransportKind(
  value: TransportKind | undefined,
): LatencyTransportKind | null {
  return value === "websocket" || value === "webtransport" ? value : null;
}

function throughput(value: ThroughputResult | null): ThroughputSnapshot | null {
  return (
    value && {
      ...value,
      serverAuthoritative: value.serverAuthoritative === true,
    }
  );
}
function latency(value: LatencyResult | null): LatencySnapshot | null {
  if (!value) return null;
  const { idleMs: _headline, ...snapshot } = value;
  return snapshot;
}
function status(result: unknown, failure: unknown): StageStatus {
  return result
    ? failure
      ? "partial"
      : "complete"
    : failure
      ? "failed"
      : "not-run";
}
function bidirectionalStatus(
  result: RunResult["bidirectional"],
  failure: unknown,
): StageStatus {
  const lanes = result ? [result.down, result.up].filter(Boolean).length : 0;
  if (lanes === 2 && !failure) return "complete";
  if (lanes > 0) return "partial";
  return failure ? "failed" : "not-run";
}
/** The first failure of each stage in that stage's own scope. */
function stageFailures(
  failures: readonly ServerFailure[],
): Partial<Record<FailureSnapshot["stage"], ServerFailure>> {
  const byStage: Partial<Record<FailureSnapshot["stage"], ServerFailure>> = {};
  for (const failure of failures)
    if ((failure.scope === "latency") === (failure.stage === "latency"))
      byStage[failure.stage] ??= failure;
  return byStage;
}
function historyText(value: string): string {
  return value.slice(0, MAX_HISTORY_TEXT_LENGTH);
}
function historyProtocol(value: string | undefined): string | null {
  return value && !value.includes("://") ? historyText(value) : null;
}

interface HistoryBuildContext {
  paths: PreparedPaths | null;
  clientBuild: string;
  wireEstimates?: WireEstimates | null;
}

export function buildHistoryRecord(
  result: RunResult,
  context: HistoryBuildContext,
  completedAt = Date.now(),
): HistoryRecord {
  const failures = stageFailures(result.multiServer.failures);
  const bidi = result.bidirectional;
  const down = throughput(result.download);
  const upload = throughput(result.upload);
  const bidiDown = throughput(bidi?.down ?? null);
  const bidiUp = throughput(bidi?.up ?? null);
  return {
    schemaVersion: HISTORY_SCHEMA_VERSION,
    multiServer: structuredClone(result.multiServer),
    outcome: result.outcome,
    id: createUuid(),
    startedAt: Math.trunc(result.startedAt),
    completedAt: Math.trunc(completedAt),
    durationMs: result.durationMs,
    stages: {
      latency: {
        status: status(result.latency, failures.latency),
        result: latency(result.latency),
        lanes: latencyLanes(result.latencyByStage),
      },
      download: {
        status: status(result.download, failures.download),
        result: down,
      },
      upload: {
        status: status(result.upload, failures.upload),
        result: upload,
      },
      bidirectional: {
        status: bidirectionalStatus(bidi, failures.bidirectional),
        down: bidiDown,
        up: bidiUp,
      },
    },
    bufferbloat: result.bufferbloat && structuredClone(result.bufferbloat),
    totalBytes: result.multiServer.servers.reduce(
      (sum, server) => sum + server.totalBytes.down + server.totalBytes.up,
      0,
    ),
    server: {
      name: historyText(
        result.multiServer.selection.map((server) => server.name).join(" + "),
      ),
      location: context.paths?.discovery.server.location
        ? historyText(context.paths.discovery.server.location)
        : null,
      engine: historyText(context.paths?.discovery.engineVersion ?? "unknown"),
    },
    transport: {
      throughput: {
        protocol: historyProtocol(
          context.paths?.throughput.probe.protocolNegotiated,
        ),
        kind: throughputTransportKind(
          context.paths?.throughput.target.transport,
        ),
      },
      latency: {
        protocol: historyProtocol(
          context.paths?.latency?.probe.protocolNegotiated,
        ),
        kind: latencyTransportKind(context.paths?.latency?.target.transport),
      },
    },
    ipVersion: context.paths?.throughput.probe.clientIpVersion ?? null,
    client: { build: historyText(context.clientBuild) },
    failures: Object.values(failures).map((failure) => ({
      stage: failure.stage,
      direction: null,
      reason: failure.reason as FailureSnapshot["reason"],
    })),
    wireEstimates: context.wireEstimates
      ? structuredClone(context.wireEstimates)
      : null,
  };
}

type Plain = Record<string, unknown>;
const object = (value: unknown): value is Plain =>
  value !== null && typeof value === "object" && !Array.isArray(value);
/** Saved counts, bytes, rates and durations are never negative. */
const numbers = (value: Plain, keys: readonly string[], nullable = false) =>
  keys.every(
    (key) =>
      (typeof value[key] === "number" && value[key] >= 0) ||
      (nullable && value[key] === null),
  );
const time = (value: unknown) =>
  typeof value === "number" && !Number.isNaN(new Date(value).getTime());
const objects = (value: Plain, keys: readonly string[]) =>
  keys.every((key) => object(value[key]));

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
const throughputShape = (value: unknown): boolean =>
  value === null ||
  (object(value) &&
    numbers(value, [
      "reportedBytesPerSec",
      "peakBytesPerSec",
      "fullAverageBytesPerSec",
      "totalBytes",
    ]));
const wireShape = (value: unknown): boolean => {
  if (!object(value) || !object(value.breakdown)) return false;
  const breakdown = value.breakdown;
  return (
    HISTORY_FAILURE_STAGES.slice(1).every(
      (stage) => breakdown[stage] === null || object(breakdown[stage]),
    ) &&
    numbers(
      value,
      ["downloadBytesPerSec", "uploadBytesPerSec", "bidirectionalBytesPerSec"],
      true,
    )
  );
};
const lanes = (value: unknown): boolean =>
  object(value) &&
  HISTORY_FAILURE_STAGES.every(
    (stage) =>
      value[stage] == null ||
      (object(value[stage]) && numbers(value[stage], ["count"])),
  );
const serverDetails = (value: unknown): boolean =>
  object(value) &&
  typeof value.latencyFocus === "string" &&
  ["selection", "participants", "servers", "intervals", "failures"].every(
    (key) => Array.isArray(value[key]),
  ) &&
  (value.selection as unknown[]).every(
    (server) => object(server) && typeof server.name === "string",
  ) &&
  (value.servers as unknown[]).every(
    (server) =>
      object(server) &&
      objects(server, [
        "server",
        "throughput",
        "latencyByStage",
        "totalBytes",
      ]) &&
      numbers(server.totalBytes as Plain, ["down", "up"]) &&
      ["download", "upload"].every((stage) => throughputShape(server[stage])),
  );

/** Records are self-authored: reading needs their version and the shape the history view dereferences. */
export function isHistoryRecord(value: unknown): value is HistoryRecord {
  if (
    !object(value) ||
    value.schemaVersion !== HISTORY_SCHEMA_VERSION ||
    !plain(value) ||
    typeof value.id !== "string" ||
    !time(value.startedAt) ||
    !time(value.completedAt) ||
    (value.startedAt as number) > (value.completedAt as number) ||
    !numbers(value, ["durationMs", "totalBytes"]) ||
    !objects(value, ["stages", "server", "transport", "client"]) ||
    !Array.isArray(value.failures) ||
    (value.multiServer !== undefined && !serverDetails(value.multiServer)) ||
    (value.wireEstimates !== null && !wireShape(value.wireEstimates))
  )
    return false;
  const stages = value.stages as Plain;
  if (!objects(stages, ["latency", "download", "upload", "bidirectional"]))
    return false;
  const latency = stages.latency as Plain,
    bidirectional = stages.bidirectional as Plain;
  return (
    lanes(latency.lanes) &&
    (latency.result === null ||
      (object(latency.result) &&
        numbers(latency.result, ["reportedMs"]) &&
        numbers(latency.result, ["probeTimeoutPct"], true) &&
        !((latency.result.probeTimeoutPct as number) > 100))) &&
    throughputShape((stages.download as Plain).result) &&
    throughputShape((stages.upload as Plain).result) &&
    throughputShape(bidirectional.down) &&
    throughputShape(bidirectional.up) &&
    typeof (value.server as Plain).name === "string"
  );
}
