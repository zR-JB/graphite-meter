import { isMultiServerResult } from "../servers/serialization";
import type { MultiServerResult } from "../servers/measurement";
import type {
  PreparedPaths,
  ReflectorTimingSummary,
  RunResult,
  StageFailure,
  ThroughputResult,
  LatencyResult,
  TransportKind,
  TerminationReason,
} from "../runner/contract";
import { createUuid, isUuid } from "../uuid";

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
  probeTimeoutPct: number | null;
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
export interface LatencyLaneSnapshot {
  reflectorTiming?: ReflectorTimingSummary;
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  center: number | null;
  jitter: number | null;
  timeoutRatio: number | null;
  accountingComplete: boolean;
  timeoutCount: number;
  unresolvedCount: number;
  sendFailureCount: number;
  count: number;
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
  schemaVersion: 3 | typeof HISTORY_SCHEMA_VERSION;
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
  wireEstimates: {
    version: 1;
    downloadBytesPerSec: number | null;
    uploadBytesPerSec: number | null;
    bidirectionalBytesPerSec: number | null;
  } | null;
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
      reportedBytesPerSec: value.reportedBytesPerSec,
      peakBytesPerSec: value.peakBytesPerSec,
      fullAverageBytesPerSec: value.fullAverageBytesPerSec,
      method: value.method,
      totalBytes: value.totalBytes,
      stabilityPct: value.stabilityPct,
      probeTimeoutPct: value.probeTimeoutPct,
      stabilityScore: value.stabilityScore,
      band: value.band,
      serverAuthoritative: value.serverAuthoritative === true,
    }
  );
}
function latency(value: LatencyResult | null): LatencySnapshot | null {
  return (
    value && {
      reportedMs: value.reportedMs,
      minMs: value.minMs,
      p50Ms: value.p50Ms,
      p95Ms: value.p95Ms,
      jitterMs: value.jitterMs,
      probeTimeoutPct: value.probeTimeoutPct,
      method: value.method,
      stabilityScore: value.stabilityScore,
      band: value.band,
    }
  );
}
function status(
  result: unknown,
  failure: StageFailure | undefined,
): StageStatus {
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
  failure: StageFailure | undefined,
): StageStatus {
  const lanes = result ? [result.down, result.up].filter(Boolean).length : 0;
  if (lanes === 2 && !failure) return "complete";
  if (lanes > 0) return "partial";
  return failure ? "failed" : "not-run";
}
function failureSnapshots(
  failures: Partial<Record<string, StageFailure>>,
): FailureSnapshot[] {
  return HISTORY_FAILURE_STAGES.flatMap((stage) => {
    const failure = failures[stage];
    return failure
      ? [
          {
            stage: failure.stage,
            direction: failure.direction ?? null,
            reason: failure.reason,
          },
        ]
      : [];
  });
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
  wireDownloadBytesPerSec?: number | null;
  wireUploadBytesPerSec?: number | null;
  wireBidirectionalBytesPerSec?: number | null;
}
export function historyLatencyLanes(
  result: LatencyResult | null,
  summaries: RunResult["latencyByStage"],
): HistoryRecord["stages"]["latency"]["lanes"] {
  return Object.fromEntries(
    HISTORY_FAILURE_STAGES.map((stage) => {
      const summary = summaries[stage];
      return [
        stage,
        summary && {
          min: summary.minMs,
          max: summary.maxMs,
          p10: summary.p10Ms,
          p90: summary.p90Ms,
          center:
            stage === "latency"
              ? (result?.reportedMs ?? summary.meanMs)
              : summary.meanMs,
          jitter: summary.jitterMs,
          ...(summary.reflectorTiming
            ? { reflectorTiming: { ...summary.reflectorTiming } }
            : {}),
          timeoutRatio: summary.probeCount
            ? summary.timeoutCount / summary.probeCount
            : null,
          accountingComplete: summary.accountingComplete,
          timeoutCount: summary.timeoutCount,
          unresolvedCount: summary.unresolvedCount,
          sendFailureCount: summary.sendFailureCount,
          count: summary.probeCount,
        },
      ];
    }),
  ) as HistoryRecord["stages"]["latency"]["lanes"];
}

export function buildHistoryRecord(
  result: RunResult,
  context: HistoryBuildContext,
  completedAt = Date.now(),
): HistoryRecord {
  const failures = result.stageFailures;
  const bidi = result.bidirectional;
  const down = throughput(result.download);
  const upload = throughput(result.upload);
  const bidiDown = throughput(bidi?.down ?? null);
  const bidiUp = throughput(bidi?.up ?? null);
  return {
    schemaVersion: HISTORY_SCHEMA_VERSION,
    ...(result.multiServer
      ? {
          multiServer: structuredClone(result.multiServer),
          outcome: result.outcome ?? "complete",
        }
      : {}),
    id: createUuid(),
    startedAt: Math.trunc(result.startedAt),
    completedAt: Math.trunc(completedAt),
    durationMs: result.durationMs,
    stages: {
      latency: {
        status: status(result.latency, failures.latency),
        result: latency(result.latency),
        lanes: historyLatencyLanes(result.latency, result.latencyByStage),
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
    bufferbloat: result.bufferbloat && { ...result.bufferbloat },
    totalBytes: result.multiServer
      ? result.multiServer.servers.reduce(
          (sum, server) => sum + server.totalBytes.down + server.totalBytes.up,
          0,
        )
      : (result.download?.totalBytes ?? 0) +
        (result.upload?.totalBytes ?? 0) +
        (bidi?.down?.totalBytes ?? 0) +
        (bidi?.up?.totalBytes ?? 0),
    server: {
      name: historyText(
        result.multiServer?.selection
          .map((server) => server.name)
          .join(" + ") ??
          context.paths?.discovery.server.name ??
          "Unknown",
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
    failures: failureSnapshots(failures),
    wireEstimates:
      context.wireDownloadBytesPerSec != null ||
      context.wireUploadBytesPerSec != null ||
      context.wireBidirectionalBytesPerSec != null
        ? {
            version: 1,
            downloadBytesPerSec: context.wireDownloadBytesPerSec ?? null,
            uploadBytesPerSec: context.wireUploadBytesPerSec ?? null,
            bidirectionalBytesPerSec:
              context.wireBidirectionalBytesPerSec ?? null,
          }
        : null,
  };
}

export function isHistoryRecord(value: unknown): value is HistoryRecord {
  if (
    !isObject(value) ||
    (value.schemaVersion !== HISTORY_SCHEMA_VERSION &&
      value.schemaVersion !== 3)
  )
    return false;
  if (
    value.schemaVersion === 3 &&
    (value.multiServer !== undefined || value.outcome !== undefined)
  )
    return false;
  if (
    value.multiServer !== undefined &&
    (!isMultiServerResult(value.multiServer) ||
      !["complete", "partial", "incomplete"].includes(String(value.outcome)))
  )
    return false;
  const record = value as Record<string, unknown>;
  const stage = (candidate: unknown): candidate is StageStatus =>
    candidate === "complete" ||
    candidate === "partial" ||
    candidate === "failed" ||
    candidate === "not-run";
  const finite = (candidate: unknown): candidate is number =>
    typeof candidate === "number" && Number.isFinite(candidate);
  const nonnegative = (candidate: unknown): candidate is number =>
    finite(candidate) && candidate >= 0;
  const epoch = (candidate: unknown): candidate is number =>
    Number.isInteger(candidate) &&
    nonnegative(candidate) &&
    !Number.isNaN(new Date(candidate).getTime());
  const percentage = (candidate: unknown): candidate is number =>
    nonnegative(candidate) && candidate <= 100;
  const unitInterval = (candidate: unknown): candidate is number =>
    nonnegative(candidate) && candidate <= 1;
  const nonnegativeOrNull = (candidate: unknown): candidate is number | null =>
    candidate === null || nonnegative(candidate);
  const text = (candidate: unknown): candidate is string =>
    typeof candidate === "string" &&
    candidate.length <= MAX_HISTORY_TEXT_LENGTH;
  const hasOnly = (
    candidate: Record<string, unknown>,
    allowed: readonly string[],
  ) => Object.keys(candidate).every((key) => allowed.includes(key));
  function validReflectorTiming(
    value: unknown,
    replies: number,
  ): value is ReflectorTimingSummary {
    if (
      !isObject(value) ||
      !hasOnly(value, [
        "sampleCount",
        "meanRawRttMs",
        "meanHandlingMs",
        "meanAdjustedRttMs",
      ])
    )
      return false;
    return (
      Number.isSafeInteger(value.sampleCount) &&
      nonnegative(value.sampleCount) &&
      value.sampleCount > 0 &&
      value.sampleCount <= replies &&
      nonnegative(value.meanRawRttMs) &&
      nonnegative(value.meanHandlingMs) &&
      nonnegative(value.meanAdjustedRttMs) &&
      value.meanHandlingMs <= value.meanRawRttMs &&
      value.meanAdjustedRttMs <= value.meanRawRttMs &&
      Math.abs(
        value.meanRawRttMs - value.meanHandlingMs - value.meanAdjustedRttMs,
      ) <=
        1e-8 * Math.max(1, value.meanRawRttMs)
    );
  }
  if (
    !hasOnly(record, [
      "schemaVersion",
      "multiServer",
      "outcome",
      "id",
      "startedAt",
      "completedAt",
      "durationMs",
      "stages",
      "bufferbloat",
      "totalBytes",
      "server",
      "transport",
      "ipVersion",
      "client",
      "failures",
      "wireEstimates",
    ])
  )
    return false;
  const method = (
    candidate: unknown,
  ): candidate is "stable-window" | "full-average" =>
    candidate === "stable-window" || candidate === "full-average";
  const band = (candidate: unknown): candidate is "low" | "medium" | "high" =>
    candidate === "low" || candidate === "medium" || candidate === "high";
  const protocol = (candidate: unknown): candidate is string | null =>
    candidate === null || (text(candidate) && !candidate.includes("://"));
  const failureStage = (
    candidate: unknown,
  ): candidate is FailureSnapshot["stage"] =>
    HISTORY_FAILURE_STAGES.includes(
      candidate as (typeof HISTORY_FAILURE_STAGES)[number],
    );
  const failureReason = (
    candidate: unknown,
  ): candidate is FailureSnapshot["reason"] =>
    candidate === "preflight-failed" ||
    candidate === "connection-lost" ||
    candidate === "timeout" ||
    candidate === "protocol-error" ||
    candidate === "internal-error" ||
    candidate === "transport-unavailable";
  const throughputSnapshot = (
    candidate: unknown,
  ): candidate is ThroughputSnapshot => {
    if (!isObject(candidate)) return false;
    return (
      hasOnly(candidate, [
        "reportedBytesPerSec",
        "peakBytesPerSec",
        "fullAverageBytesPerSec",
        "method",
        "totalBytes",
        "stabilityPct",
        "probeTimeoutPct",
        "stabilityScore",
        "band",
        "serverAuthoritative",
      ]) &&
      nonnegative(candidate.reportedBytesPerSec) &&
      nonnegative(candidate.peakBytesPerSec) &&
      nonnegative(candidate.fullAverageBytesPerSec) &&
      method(candidate.method) &&
      nonnegative(candidate.totalBytes) &&
      percentage(candidate.stabilityPct) &&
      (candidate.probeTimeoutPct === null ||
        percentage(candidate.probeTimeoutPct)) &&
      unitInterval(candidate.stabilityScore) &&
      band(candidate.band) &&
      typeof candidate.serverAuthoritative === "boolean"
    );
  };
  const latencySnapshot = (
    candidate: unknown,
  ): candidate is LatencySnapshot => {
    if (!isObject(candidate)) return false;
    return (
      hasOnly(candidate, [
        "reportedMs",
        "minMs",
        "p50Ms",
        "p95Ms",
        "jitterMs",
        "probeTimeoutPct",
        "method",
        "stabilityScore",
        "band",
      ]) &&
      nonnegative(candidate.reportedMs) &&
      nonnegativeOrNull(candidate.minMs) &&
      nonnegativeOrNull(candidate.p50Ms) &&
      nonnegativeOrNull(candidate.p95Ms) &&
      nonnegativeOrNull(candidate.jitterMs) &&
      (candidate.probeTimeoutPct === null ||
        percentage(candidate.probeTimeoutPct)) &&
      method(candidate.method) &&
      unitInterval(candidate.stabilityScore) &&
      band(candidate.band)
    );
  };
  const lane = (candidate: unknown): candidate is LatencyLaneSnapshot => {
    if (!isObject(candidate)) return false;
    return (
      hasOnly(candidate, [
        "min",
        "max",
        "p10",
        "p90",
        "center",
        "jitter",
        "timeoutRatio",
        "reflectorTiming",
        "unresolvedCount",
        "sendFailureCount",
        "accountingComplete",
        "timeoutCount",
        "count",
      ]) &&
      nonnegativeOrNull(candidate.min) &&
      nonnegativeOrNull(candidate.max) &&
      nonnegativeOrNull(candidate.p10) &&
      nonnegativeOrNull(candidate.p90) &&
      nonnegativeOrNull(candidate.center) &&
      nonnegativeOrNull(candidate.jitter) &&
      typeof candidate.accountingComplete === "boolean" &&
      Number.isSafeInteger(candidate.count) &&
      nonnegative(candidate.count) &&
      Number.isSafeInteger(candidate.timeoutCount) &&
      nonnegative(candidate.timeoutCount) &&
      candidate.timeoutCount <= candidate.count &&
      candidate.timeoutRatio ===
        (candidate.count ? candidate.timeoutCount / candidate.count : null) &&
      Number.isSafeInteger(candidate.unresolvedCount) &&
      nonnegative(candidate.unresolvedCount) &&
      Number.isSafeInteger(candidate.sendFailureCount) &&
      nonnegative(candidate.sendFailureCount) &&
      (candidate.reflectorTiming === undefined ||
        validReflectorTiming(
          candidate.reflectorTiming,
          candidate.count - candidate.timeoutCount,
        ))
    );
  };
  const throughputStage = (
    candidate: unknown,
  ): candidate is {
    status: StageStatus;
    result: ThroughputSnapshot | null;
  } => {
    if (
      !isObject(candidate) ||
      !hasOnly(candidate, ["status", "result"]) ||
      !stage(candidate.status)
    )
      return false;
    if (candidate.result === null)
      return candidate.status === "failed" || candidate.status === "not-run";
    return (
      (candidate.status === "complete" || candidate.status === "partial") &&
      throughputSnapshot(candidate.result)
    );
  };
  const stages = record.stages;
  if (
    !isObject(stages) ||
    !hasOnly(stages, ["latency", "download", "upload", "bidirectional"])
  )
    return false;
  const latencyStage = stages.latency;
  const downStage = stages.download;
  const uploadStage = stages.upload;
  const bidiStage = stages.bidirectional;
  if (!throughputStage(downStage) || !throughputStage(uploadStage))
    return false;
  if (
    !isObject(bidiStage) ||
    !hasOnly(bidiStage, ["status", "down", "up"]) ||
    !stage(bidiStage.status) ||
    (bidiStage.down !== null && !throughputSnapshot(bidiStage.down)) ||
    (bidiStage.up !== null && !throughputSnapshot(bidiStage.up))
  )
    return false;
  const bidiLaneCount = [bidiStage.down, bidiStage.up].filter(
    (candidate) => candidate !== null,
  ).length;
  if (
    (bidiLaneCount === 2 &&
      bidiStage.status !== "complete" &&
      bidiStage.status !== "partial") ||
    (bidiLaneCount === 1 && bidiStage.status !== "partial") ||
    (bidiLaneCount === 0 &&
      bidiStage.status !== "failed" &&
      bidiStage.status !== "not-run")
  )
    return false;
  if (
    !isObject(latencyStage) ||
    !hasOnly(latencyStage, ["status", "result", "lanes"]) ||
    !stage(latencyStage.status) ||
    (latencyStage.result !== null && !latencySnapshot(latencyStage.result)) ||
    !isObject(latencyStage.lanes) ||
    !hasOnly(latencyStage.lanes, [
      "latency",
      "download",
      "upload",
      "bidirectional",
    ])
  )
    return false;
  if (
    (latencyStage.result === null &&
      latencyStage.status !== "failed" &&
      latencyStage.status !== "not-run") ||
    (latencyStage.result !== null &&
      latencyStage.status !== "complete" &&
      latencyStage.status !== "partial")
  )
    return false;
  for (const key of [
    "latency",
    "download",
    "upload",
    "bidirectional",
  ] as const) {
    const candidate = latencyStage.lanes[key];
    if (candidate !== null && !lane(candidate)) return false;
  }
  if (
    !epoch(record.startedAt) ||
    !epoch(record.completedAt) ||
    record.completedAt < record.startedAt ||
    !nonnegative(record.durationMs) ||
    !nonnegative(record.totalBytes) ||
    !isUuid(record.id)
  )
    return false;
  const measuredBytes = record.multiServer
    ? (record.multiServer as MultiServerResult).servers.reduce(
        (sum, server) => sum + server.totalBytes.down + server.totalBytes.up,
        0,
      )
    : (downStage.result?.totalBytes ?? 0) +
      (uploadStage.result?.totalBytes ?? 0) +
      (bidiStage.down?.totalBytes ?? 0) +
      (bidiStage.up?.totalBytes ?? 0);
  if (record.totalBytes !== measuredBytes) return false;
  const server = record.server;
  if (
    !isObject(server) ||
    !hasOnly(server, ["name", "location", "engine"]) ||
    !text(server.name) ||
    (server.location !== null && !text(server.location)) ||
    !text(server.engine)
  )
    return false;
  const transport = record.transport;
  const transportEntry = (
    candidate: unknown,
    kind: (candidate: unknown) => boolean,
  ): boolean =>
    isObject(candidate) &&
    hasOnly(candidate, ["protocol", "kind"]) &&
    protocol(candidate.protocol) &&
    (candidate.kind === null || kind(candidate.kind));
  if (
    !isObject(transport) ||
    !hasOnly(transport, ["throughput", "latency"]) ||
    !transportEntry(
      transport.throughput,
      (kind) =>
        kind === "fetch-stream" ||
        kind === "webtransport" ||
        kind === "webtransport-datagram",
    ) ||
    !transportEntry(
      transport.latency,
      (kind) => kind === "websocket" || kind === "webtransport",
    )
  )
    return false;
  if (
    record.ipVersion !== null &&
    record.ipVersion !== 4 &&
    record.ipVersion !== 6
  )
    return false;
  const client = record.client;
  if (!isObject(client) || !hasOnly(client, ["build"]) || !text(client.build))
    return false;
  if (
    !Array.isArray(record.failures) ||
    record.failures.length > HISTORY_FAILURE_STAGES.length
  )
    return false;
  const failedStages = new Set<FailureSnapshot["stage"]>();
  for (const failure of record.failures) {
    if (
      !isObject(failure) ||
      !hasOnly(failure, ["stage", "direction", "reason"]) ||
      !failureStage(failure.stage) ||
      !failureReason(failure.reason) ||
      (failure.direction !== null &&
        failure.direction !== "down" &&
        failure.direction !== "up") ||
      failedStages.has(failure.stage)
    )
      return false;
    failedStages.add(failure.stage);
  }
  const bufferbloat = record.bufferbloat;
  if (
    bufferbloat !== null &&
    (!isObject(bufferbloat) ||
      !hasOnly(bufferbloat, ["idleMs", "loadedMs", "increaseMs", "grade"]) ||
      !nonnegative(bufferbloat.idleMs) ||
      !nonnegative(bufferbloat.loadedMs) ||
      !nonnegative(bufferbloat.increaseMs) ||
      typeof bufferbloat.grade !== "string" ||
      !["A", "B", "C", "D", "F"].includes(bufferbloat.grade))
  )
    return false;
  const wire = record.wireEstimates;
  return (
    wire === null ||
    (isObject(wire) &&
      hasOnly(wire, [
        "version",
        "downloadBytesPerSec",
        "uploadBytesPerSec",
        "bidirectionalBytesPerSec",
      ]) &&
      wire.version === 1 &&
      nonnegativeOrNull(wire.downloadBytesPerSec) &&
      nonnegativeOrNull(wire.uploadBytesPerSec) &&
      nonnegativeOrNull(wire.bidirectionalBytesPerSec))
  );
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
