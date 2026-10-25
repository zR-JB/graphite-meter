// Saved results: the schema 6 record, how History reads and orders rows, and the upgrade of older rows.
import type { ThroughputResult } from "../../engine/measure/aggregate";
import type { AddedLatency, LatencyResult, LatencySummary } from "../../engine/measure/latency";
import type { WireModel } from "../../engine/measure/overhead";
import { epochMs } from "../../engine/run/clock";
import {
  graded,
  incoherence,
  type MultiServerResult,
  type Outcome,
  type RunResult,
  type StagePlans,
  type StageStatus,
} from "../../engine/run/result";
import { STAGES, type Stage, type TransferStage } from "../../engine/run/schedule";
import type { HttpProtocol } from "../../engine/types";
import { isRecord } from "../../lib/decode";

export const HISTORY_LIMIT = 2_000;

export interface HistoryRecord {
  schemaVersion: 6;
  /** A v4 UUID when this client saved it; older rows may carry any text. */
  id: string;
  completedAt: number;
  build: string;
  result: RunResult;
}

/** A finished run as History keeps it: one its own evidence contradicts is saved as incomplete. */
export function historyRecord(run: { result: RunResult; plans: StagePlans }): HistoryRecord {
  const record: HistoryRecord = {
    schemaVersion: 6,
    id: createUuid(),
    completedAt: epochMs(),
    build: BUILD.clientVersion,
    result: structuredClone(run.result),
  };
  const problems = incoherence(record.result, run.plans);
  if (problems.length && record.result.outcome !== "incomplete") {
    console.error("Incoherent result saved as incomplete:", problems);
    record.result.outcome = "incomplete";
  }
  return record;
}

/** A v4 UUID; randomUUID needs a secure context, which plain-HTTP LAN deployments are not. */
function createUuid(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = bytes.toHex();
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** Stored leaves are finite numbers, text up to 2,048 characters, booleans or null; at most 512 entries, 8 deep. */
function plain(value: unknown, depth = 0): boolean {
  if (value == null || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (typeof value === "string") return value.length <= 2_048;
  if (typeof value !== "object" || depth > 8) return false;
  const entries = Array.isArray(value) ? value : Object.values(value);
  return entries.length <= 512 && entries.every((entry) => plain(entry, depth + 1));
}

// Checked in place rather than decoded, and only the fields History's views dereference.
type Check = (value: unknown) => boolean;
const finite: Check = (value) => Number.isFinite(value);
const text: Check = (value) => typeof value === "string";
const nullable =
  (check: Check): Check =>
  (value) =>
    value == null || check(value);
const orNull =
  (check: Check): Check =>
  (value) =>
    value === null || check(value);
const among =
  (values: readonly unknown[]): Check =>
  (value) =>
    values.includes(value);
const list =
  (check: Check): Check =>
  (value) =>
    Array.isArray(value) && value.every((entry) => check(entry));
const shape = (fields: Record<string, Check>): Check => {
  const checks = Object.entries(fields);
  return (value) => isRecord(value) && checks.every(([key, check]) => check(value[key]));
};
const perStage = (check: Check) => shape(Object.fromEntries(STAGES.map((stage) => [stage, check])));
const STATUSES: readonly StageStatus[] = ["complete", "partial", "failed", "not-run"];
const PROTOCOLS: readonly HttpProtocol[] = ["http1", "http2", "http3", "negotiated"];

/** The fields History's views dereference, as this build saves them, or (`upgraded` false) as any release did. */
function recordShape(upgraded: boolean): Check {
  const rate = nullable(
    shape({
      reportedBytesPerSec: finite,
      ...(upgraded ? { peakBytesPerSec: finite } : {}),
      totalBytes: finite,
      stabilityPct: finite,
      wire: nullable(
        shape({
          totalMultiplier: finite,
          mtuBytes: finite,
          factors: list(shape({ label: text, contributionPct: finite })),
        }),
      ),
    }),
  );
  const measured = {
    latency: nullable(
      shape({ reportedMs: finite, ...(upgraded ? { stabilityPct: orNull(finite) } : {}) }),
    ),
    latencyByStage: perStage(nullable(shape({ p50Ms: nullable(finite), probeCount: finite }))),
    addedLatency: nullable(
      shape({
        download: nullable(finite),
        upload: nullable(finite),
        bidirectional: nullable(finite),
      }),
    ),
    download: rate,
    upload: rate,
    bidirectional: nullable(shape({ down: rate, up: rate })),
  };
  const stages = upgraded ? perStage(among(STATUSES)) : isRecord;
  return shape({
    schemaVersion: (value) => value === (upgraded ? 6 : 5),
    id: text,
    completedAt: (value) => typeof value === "number" && Math.abs(value) <= 8.64e15,
    build: text,
    ...(upgraded ? {} : { engine: text }),
    result: shape({
      ...measured,
      outcome: among(["complete", "partial", "incomplete"] satisfies Outcome[]),
      stages,
      durationMs: finite,
      multiServer: shape({
        latencyFocus: text,
        selection: list(shape({ id: text, name: text, url: text })),
        servers: list(
          shape({
            ...measured,
            server: shape({ id: text }),
            ...(upgraded ? { engineVersion: nullable(text) } : {}),
            throughput: shape({
              transport: text,
              ...(upgraded ? { protocol: among(PROTOCOLS) } : {}),
            }),
            latencyTarget: nullable(shape({ transport: text })),
            totalBytes: shape({ down: finite, up: finite }),
            stages,
          }),
        ),
        failures: list(
          shape({
            serverId: text,
            stage: (value) => STAGES.includes(value as Stage),
            reason: text,
          }),
        ),
      }),
    }),
  });
}
const readable = recordShape(true);
const stored = recordShape(false);

/** A readable record, its stages judged again: older builds read a stage by its sum alone, not by who was missing. */
export function readHistoryRecord(value: unknown): HistoryRecord | null {
  if (!isRecord(value) || !plain(value) || !readable(value)) return null;
  const record = value as unknown as HistoryRecord;
  return { ...record, result: graded(record.result) };
}

/** A schema 4 or 5 row in this build's form; null when its own release could not read it either. */
export function upgradeRecord(value: unknown): HistoryRecord | null {
  if (!isRecord(value) || !plain(value)) return null;
  try {
    const saved = value.schemaVersion === 4 && liftable(value) ? lift(value) : value;
    if (!stored(saved)) return null;
    // Stored as its release saved it; reading judges it again.
    const record = current(saved as Schema5);
    return plain(record) && readable(record) ? record : null;
  } catch {
    return null;
  }
}

// Schema 4 kept the version as the browser names it; a version never recorded was the browser's to negotiate.
const NAMED: Record<string, HttpProtocol> = {
  http1: "http1",
  "http/1.1": "http1",
  http2: "http2",
  h2: "http2",
  http3: "http3",
  h3: "http3",
};

// Older builds saved no peak without a 500 ms window; this build's peak never falls below the headline.
const lane = <T extends Omit<ThroughputResult, "quietMs"> | null>(saved: T): T =>
  saved && {
    ...saved,
    peakBytesPerSec: Number.isFinite(saved.peakBytesPerSec)
      ? saved.peakBytesPerSec
      : saved.reportedBytesPerSec,
  };
const latency = (saved: LatencyResult | null): LatencyResult | null =>
  saved && { ...saved, stabilityPct: saved.stabilityPct ?? null };
const stageStatuses = (saved: Record<Stage, StageStatus>) =>
  Object.fromEntries(
    STAGES.map((stage) => [stage, STATUSES.includes(saved[stage]) ? saved[stage] : "not-run"]),
  ) as Record<Stage, StageStatus>;

type Schema5 = Omit<HistoryRecord, "schemaVersion"> & { schemaVersion: 5; engine: string };

/** Older builds left out a peak, wire model or latency stability, named HTTP versions, and kept one engine version. */
function current({ result, engine, ...record }: Schema5): HistoryRecord {
  const { download, upload, bidirectional, multiServer } = result;
  const owner =
    multiServer.servers.find(({ server }) => server.id === multiServer.latencyFocus) ??
    multiServer.servers[0];
  return {
    ...record,
    schemaVersion: 6,
    result: {
      ...result,
      latency: latency(result.latency),
      download: download && { ...lane(download), wire: download.wire ?? null },
      upload: upload && { ...lane(upload), wire: upload.wire ?? null },
      bidirectional: bidirectional && {
        ...bidirectional,
        down: lane(bidirectional.down),
        up: lane(bidirectional.up),
        wire: bidirectional.wire ?? null,
      },
      stages: stageStatuses(result.stages),
      multiServer: {
        ...multiServer,
        servers: multiServer.servers.map((server) => ({
          ...server,
          ...(server === owner && engine !== "unknown" ? { engineVersion: engine } : {}),
          throughput: {
            ...server.throughput,
            protocol: Object.hasOwn(NAMED, server.throughput.protocol)
              ? NAMED[server.throughput.protocol]!
              : "negotiated",
          },
          latency: latency(server.latency),
          download: lane(server.download),
          upload: lane(server.upload),
          bidirectional: server.bidirectional && {
            ...server.bidirectional,
            down: lane(server.bidirectional.down),
            up: lane(server.bidirectional.up),
          },
          stages: stageStatuses(server.stages),
        })),
      },
    },
  };
}

/** Schema 4 as older builds saved it: one server's stages side by side, or with a result per server. */
interface Schema4 {
  id: string;
  completedAt: number;
  startedAt: number;
  durationMs: number;
  outcome?: Outcome | null;
  multiServer?: MultiServerResult | null;
  stages: {
    latency: {
      status: StageStatus;
      result: LatencyResult | null;
      lanes: Record<Stage, Lane4 | null>;
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
  transport: Record<"throughput" | "latency", { protocol: string | null; kind: string | null }>;
  ipVersion: 4 | 6 | null;
  client: { build: string };
  wireEstimates:
    | ({ breakdown: Record<TransferStage, Omit<WireModel, "totalMultiplier"> | null> } & Record<
        `${TransferStage}BytesPerSec`,
        number | null
      >)
    | null;
}
interface Lane4 {
  reflectorTiming?: LatencySummary["reflectorTiming"];
  accountingComplete: boolean;
  count: number;
  timeoutCount: number;
  unresolvedCount: number;
  sendFailureCount: number;
  min: number | null;
  max: number | null;
  p10: number | null;
  center: number | null;
  p90: number | null;
  p95?: number | null;
  jitter: number | null;
}

const liftable = shape({
  id: text,
  stages: shape({
    latency: shape({ lanes: perStage(nullable(isRecord)) }),
    download: isRecord,
    upload: isRecord,
    bidirectional: isRecord,
  }),
  server: isRecord,
  client: isRecord,
  transport: shape({ throughput: isRecord, latency: isRecord }),
  wireEstimates: nullable(shape({ breakdown: isRecord })),
  multiServer: nullable(shape({ servers: list(isRecord) })),
}) as (value: unknown) => value is Schema4;

const summary = (lane: Lane4 | null): LatencySummary | null =>
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

function lift(saved: Schema4): Schema5 {
  const { stages, wireEstimates } = saved;
  // Schema 4 kept the estimated wire rate; the model keeps its ratio to the measured rate.
  const wire = (stage: TransferStage, measuredRate: number): WireModel | null => {
    const breakdown = wireEstimates?.breakdown[stage];
    const estimated = wireEstimates?.[`${stage}BytesPerSec`];
    return breakdown && estimated && measuredRate
      ? { ...breakdown, totalMultiplier: estimated / measuredRate }
      : null;
  };
  const transfer = (stage: "download" | "upload") => {
    const result = stages[stage].result;
    return result && { ...result, wire: wire(stage, result.reportedBytesPerSec) };
  };
  const { down, up, status } = stages.bidirectional;
  const results = {
    latency: stages.latency.result,
    latencyByStage: Object.fromEntries(
      STAGES.map((stage) => [stage, summary(stages.latency.lanes[stage])]),
    ) as Record<Stage, LatencySummary | null>,
    addedLatency: saved.bufferbloat?.addedMs
      ? { download: null, upload: null, bidirectional: null, ...saved.bufferbloat.addedMs }
      : null,
    download: transfer("download"),
    upload: transfer("upload"),
    bidirectional:
      status === "not-run"
        ? null
        : {
            down,
            up,
            wire:
              down && up
                ? wire("bidirectional", down.reportedBytesPerSec + up.reportedBytesPerSec)
                : null,
          },
  };
  const statuses = Object.fromEntries(
    STAGES.map((stage) => [stage, stages[stage].status]),
  ) as Record<Stage, StageStatus>;
  const settled = Object.values(statuses);
  const server = {
    id: "self",
    url: "",
    name: saved.server.name,
    ...(saved.server.location ? { location: saved.server.location } : {}),
  };
  const { throughput, latency } = saved.transport;
  return {
    schemaVersion: 5,
    id: saved.id,
    completedAt: saved.completedAt,
    build: saved.client.build,
    engine: saved.server.engine,
    result: {
      ...results,
      multiServer: saved.multiServer
        ? {
            ...saved.multiServer,
            servers: saved.multiServer.servers.map((entry) => ({ ...entry, stages: statuses })),
          }
        : {
            selection: [server],
            participants: [server.id],
            latencyFocus: server.id,
            servers: [
              {
                server,
                throughput: {
                  origin: "",
                  transport: throughput.kind ?? "",
                  protocol: (throughput.protocol ?? "") as HttpProtocol,
                  ...(saved.ipVersion ? { clientIpVersion: saved.ipVersion } : {}),
                },
                latencyTarget: latency.kind ? { origin: "", transport: latency.kind } : null,
                ...results,
                totalBytes: { down: saved.totalBytes, up: 0 },
                stages: statuses,
              },
            ],
            intervals: [],
            omittedIntervals: 0,
            // Schema 4 failures named no server or time: a lifted stage keeps its status, not its cause.
            failures: [],
          },
      outcome:
        saved.outcome ??
        (settled.includes("failed")
          ? "incomplete"
          : settled.includes("partial")
            ? "partial"
            : "complete"),
      stages: statuses,
      startedAt: saved.startedAt,
      durationMs: saved.durationMs,
    },
  };
}

export type HistorySort = "date" | "download" | "upload" | "bidirectional" | "idle" | "loaded";

/** Latency reads best lowest first; dates and rates highest first. */
export const naturalDescending = (sort: HistorySort) => sort !== "idle" && sort !== "loaded";

/** What History's list keeps of a record: a few hundred bytes of its kilobytes; a record is read whole only as it opens. */
export interface HistoryEntry {
  id: string;
  completedAt: number;
  outcome: Outcome;
  servers: number;
  server: string;
  stages: Record<Stage, StageStatus>;
  jitterMs: number | null;
  addedLatency: AddedLatency | null;
  /** The load whose median the Loaded column shows: the highest, the first of equals. */
  loadedStage: TransferStage | null;
  /** The one direction a bidirectional stage kept, when it kept only one. */
  lone: "down" | "up" | null;
  keys: Record<HistorySort, number | null>;
}

const LOADED = ["download", "upload", "bidirectional"] as const;

export function historyEntry(record: HistoryRecord): HistoryEntry {
  const { result } = record;
  const down = result.bidirectional?.down?.reportedBytesPerSec;
  const up = result.bidirectional?.up?.reportedBytesPerSec;
  // The highest loaded median, as the detail's lanes center on it, with or without idle latency.
  let loadedStage: TransferStage | null = null;
  for (const stage of LOADED) {
    const median = result.latencyByStage[stage]?.p50Ms;
    if (
      median != null &&
      (loadedStage === null || median > result.latencyByStage[loadedStage]!.p50Ms!)
    )
      loadedStage = stage;
  }
  const selection = result.multiServer.selection;
  return {
    id: record.id,
    completedAt: record.completedAt,
    outcome: result.outcome,
    servers: selection.length,
    server: selection[0]?.name ?? "",
    stages: result.stages,
    jitterMs: result.latency?.jitterMs ?? null,
    addedLatency: result.addedLatency,
    loadedStage,
    lone: (down == null) === (up == null) ? null : down == null ? "up" : "down",
    keys: {
      date: record.completedAt,
      download: result.download?.reportedBytesPerSec ?? null,
      upload: result.upload?.reportedBytesPerSec ?? null,
      // A large surviving lane must not outrank a complete bidirectional result.
      bidirectional: down != null && up != null ? down + up : null,
      idle: result.latency?.reportedMs ?? null,
      loaded: loadedStage && result.latencyByStage[loadedStage]!.p50Ms,
    },
  };
}

/** Missing values last in either direction; equal ones newest first, then by id. */
export function sortHistory(
  entries: readonly HistoryEntry[],
  sort: HistorySort,
  descending: boolean,
): HistoryEntry[] {
  return entries.toSorted((a, b) => {
    const [x, y] = [a.keys[sort], b.keys[sort]];
    if (x === y) return b.completedAt - a.completedAt || b.id.localeCompare(a.id);
    if (x === null || y === null) return x === null ? 1 : -1;
    return descending ? y - x : x - y;
  });
}

/** "now" within a minute, "<n> min ago" within the hour, then nothing. */
export function recentCompletion(completedAt: number, now: number): string | null {
  const minutes = Math.floor(Math.max(0, now - completedAt) / 60_000);
  return minutes >= 60 ? null : minutes ? `${minutes} min ago` : "now";
}
