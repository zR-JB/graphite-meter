import type {
  InfraInfo,
  RunResult,
  StageFailure,
  ThroughputResult,
  LatencyResult,
  TransportKind,
  TerminationReason,
} from "../runner/contract";

export const HISTORY_SCHEMA_VERSION = 1 as const;
export const HISTORY_LIMIT = 2_000 as const;
const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export type StageStatus = "complete" | "partial" | "failed" | "not-run";
export interface FailureSnapshot {
  stage: "latency" | "download" | "upload" | "bidirectional";
  direction: "down" | "up" | null;
  reason: Exclude<TerminationReason, "user-abort">;
}
export interface ThroughputSnapshot {
  meanBytesPerSec: number;
  reportedBytesPerSec: number;
  peakBytesPerSec: number;
  fullAverageBytesPerSec: number;
  method: "stable-window" | "full-average";
  totalBytes: number;
  stabilityPct: number;
  packetLossPct: number;
  stabilityScore: number;
  band: "low" | "medium" | "high";
  serverAuthoritative: boolean;
}
export interface LatencySnapshot {
  reportedMs: number;
  minMs: number;
  p50Ms: number;
  p95Ms: number;
  jitterMs: number;
  packetLossPct: number;
  method: "stable-window" | "full-average";
  stabilityScore: number;
  band: "low" | "medium" | "high";
}
export interface LatencyLaneSnapshot {
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  center: number | null;
  jitter: number | null;
  lossRatio: number;
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
export interface HistoryRecordV1 {
  schemaVersion: 1;
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
      meanBytesPerSec: value.meanBytesPerSec,
      reportedBytesPerSec: value.reportedBytesPerSec,
      peakBytesPerSec: value.peakBytesPerSec,
      fullAverageBytesPerSec: value.fullAverageBytesPerSec,
      method: value.method,
      totalBytes: value.totalBytes,
      stabilityPct: value.stabilityPct,
      packetLossPct: value.packetLossPct,
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
      packetLossPct: value.packetLossPct,
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
  return Object.values(failures)
    .filter(Boolean)
    .map((failure) => ({
      stage: failure!.stage,
      direction: failure!.direction ?? null,
      reason: failure!.reason,
    }));
}
function id(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function")
    return crypto.randomUUID();
  const bytes = new Uint8Array(16);
  if (
    typeof crypto !== "undefined" &&
    typeof crypto.getRandomValues === "function"
  )
    crypto.getRandomValues(bytes);
  else
    for (let index = 0; index < bytes.length; index++)
      bytes[index] = Math.floor(Math.random() * 256);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = [...bytes]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export interface HistoryBuildContext {
  infra: InfraInfo | null;
  clientBuild: string;
  engineVersion: string;
  latencyLanes?: Partial<
    Record<
      "latency" | "download" | "upload" | "bidirectional",
      LatencyLaneSnapshot
    >
  >;
  wireDownloadBytesPerSec?: number | null;
  wireUploadBytesPerSec?: number | null;
  wireBidirectionalBytesPerSec?: number | null;
}
export function buildHistoryRecord(
  result: RunResult,
  context: HistoryBuildContext,
  completedAt = Date.now(),
): HistoryRecordV1 {
  const failures = result.stageFailures;
  const bidi = result.bidirectional;
  const down = throughput(result.download);
  const upload = throughput(result.upload);
  const bidiDown = throughput(bidi?.down ?? null);
  const bidiUp = throughput(bidi?.up ?? null);
  return {
    schemaVersion: 1,
    id: id(),
    startedAt: Math.trunc(result.startedAt),
    completedAt,
    durationMs: result.durationMs,
    stages: {
      latency: {
        status: status(result.latency, failures.latency),
        result: latency(result.latency),
        lanes: {
          latency: context.latencyLanes?.latency ?? null,
          download: context.latencyLanes?.download ?? null,
          upload: context.latencyLanes?.upload ?? null,
          bidirectional: context.latencyLanes?.bidirectional ?? null,
        },
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
    totalBytes:
      (result.download?.totalBytes ?? 0) +
      (result.upload?.totalBytes ?? 0) +
      (bidi?.down?.totalBytes ?? 0) +
      (bidi?.up?.totalBytes ?? 0),
    server: {
      name: context.infra?.server.name ?? "Unknown",
      location: context.infra?.server.location ?? null,
      engine: context.engineVersion,
    },
    transport: {
      throughput: {
        protocol: context.infra?.protocolNegotiated ?? null,
        kind: throughputTransportKind(
          context.infra?.selectedThroughputTransport,
        ),
      },
      latency: {
        protocol: context.infra?.latencyProtocolNegotiated ?? null,
        kind: latencyTransportKind(context.infra?.selectedLatencyTransport),
      },
    },
    ipVersion: context.infra?.clientIpVersion ?? null,
    client: { build: context.clientBuild },
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

export function isHistoryRecord(value: unknown): value is HistoryRecordV1 {
  if (!isObject(value)) return false;
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
  const text = (candidate: unknown, max = 256): candidate is string =>
    typeof candidate === "string" && candidate.length <= max;
  const hasOnly = (
    candidate: Record<string, unknown>,
    allowed: readonly string[],
  ) => Object.keys(candidate).every((key) => allowed.includes(key));
  if (
    !hasOnly(record, [
      "schemaVersion",
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
    candidate === null || (text(candidate, 32) && !candidate.includes("://"));
  const failureStage = (
    candidate: unknown,
  ): candidate is FailureSnapshot["stage"] =>
    candidate === "latency" ||
    candidate === "download" ||
    candidate === "upload" ||
    candidate === "bidirectional";
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
        "meanBytesPerSec",
        "reportedBytesPerSec",
        "peakBytesPerSec",
        "fullAverageBytesPerSec",
        "method",
        "totalBytes",
        "stabilityPct",
        "packetLossPct",
        "stabilityScore",
        "band",
        "serverAuthoritative",
      ]) &&
      nonnegative(candidate.meanBytesPerSec) &&
      nonnegative(candidate.reportedBytesPerSec) &&
      nonnegative(candidate.peakBytesPerSec) &&
      nonnegative(candidate.fullAverageBytesPerSec) &&
      method(candidate.method) &&
      nonnegative(candidate.totalBytes) &&
      percentage(candidate.stabilityPct) &&
      percentage(candidate.packetLossPct) &&
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
        "packetLossPct",
        "method",
        "stabilityScore",
        "band",
      ]) &&
      nonnegative(candidate.reportedMs) &&
      nonnegative(candidate.minMs) &&
      nonnegative(candidate.p50Ms) &&
      nonnegative(candidate.p95Ms) &&
      nonnegative(candidate.jitterMs) &&
      percentage(candidate.packetLossPct) &&
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
        "lossRatio",
        "count",
      ]) &&
      nonnegativeOrNull(candidate.min) &&
      nonnegativeOrNull(candidate.max) &&
      nonnegativeOrNull(candidate.p10) &&
      nonnegativeOrNull(candidate.p90) &&
      nonnegativeOrNull(candidate.center) &&
      nonnegativeOrNull(candidate.jitter) &&
      unitInterval(candidate.lossRatio) &&
      Number.isInteger(candidate.count) &&
      nonnegative(candidate.count)
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
    record.schemaVersion !== 1 ||
    typeof record.id !== "string" ||
    !UUID.test(record.id)
  )
    return false;
  const measuredBytes =
    (downStage.result?.totalBytes ?? 0) +
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
    record.failures.some(
      (failure) =>
        !isObject(failure) ||
        !hasOnly(failure, ["stage", "direction", "reason"]) ||
        !failureStage(failure.stage) ||
        !failureReason(failure.reason) ||
        (failure.direction !== null &&
          failure.direction !== "down" &&
          failure.direction !== "up") ||
        !text(failure.reason, 128),
    )
  )
    return false;
  const bufferbloat = record.bufferbloat;
  if (
    bufferbloat !== null &&
    (!isObject(bufferbloat) ||
      !hasOnly(bufferbloat, ["idleMs", "loadedMs", "increaseMs", "grade"]) ||
      !nonnegative(bufferbloat.idleMs) ||
      !nonnegative(bufferbloat.loadedMs) ||
      !nonnegative(bufferbloat.increaseMs) ||
      !text(bufferbloat.grade, 1) ||
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
