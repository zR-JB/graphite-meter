import type {
  FlowDirection,
  TransportRole,
  ThroughputResult,
  ReceiverCheckpoint,
  RunnerConfig,
  StageLatencySummary,
  LatencyResult,
  BufferbloatGrade,
} from "../runner/contract";
import type { ServerIdentity } from "./catalog";
import {
  MIN_PARTIAL_TRANSFER_EVIDENCE_MS,
  RunAccumulator,
} from "../runner/evaluation";
import {
  bandForState,
  isStillStable,
  transferConfidence,
  type ConfidenceScore,
  type LatencyConfidenceScore,
} from "../runner/adaptive";
import { FixedRateBuckets } from "../runner/controlBuckets";
import { TRANSFER_CONFIDENCE_BUCKETS } from "../runner/adaptive";

export type TransferStage = Exclude<TransportRole, "latency">;
export interface ComponentWindow {
  serverId: string;
  bytes: number;
  durationMs: number;
  bytesPerSec: number;
  clock: "client-monotonic" | "receiver";
  startBytes: number;
  endBytes: number;
  startNanos?: number;
  endNanos?: number;
  startRequestMs?: number;
  startResponseMs?: number;
  endRequestMs?: number;
  endResponseMs?: number;
}
export interface AggregateWindow {
  startMs: number;
  endMs: number;
  down: ComponentWindow[] | null;
  up: ComponentWindow[] | null;
  downBytesPerSec: number | null;
  upBytesPerSec: number | null;
}
export interface AggregationInterval {
  id: number;
  stage: TransferStage;
  participants: string[];
  startMs: number;
  endMs: number;
  complete: boolean;
  reason: "stage-start" | "dropout" | "evidence-resumed";
  full: AggregateWindow | null;
  headline: AggregateWindow | null;
}
export interface ServerFailure {
  serverId: string;
  stage: TransportRole;
  atMs: number;
  scope: "throughput" | "latency";
  reason: string;
  message: string;
}
export interface ServerMeasurementSummary {
  server: ServerIdentity;
  throughput: {
    origin: string;
    transport: string;
    protocol: string;
    browserProtocol?: string;
    clientIpVersion?: 4 | 6;
  };
  latencyTarget: { origin: string; transport: string } | null;
  latency: LatencyResult | null;
  latencyByStage: Record<TransportRole, StageLatencySummary | null>;
  bufferbloat: BufferbloatGrade | null;
  download: ThroughputResult | null;
  upload: ThroughputResult | null;
  bidirectional: {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
  } | null;
  totalBytes: Record<FlowDirection, number>;
}
export interface MultiServerResult {
  selection: ServerIdentity[];
  participants: string[];
  latencyFocus: string;
  servers: ServerMeasurementSummary[];
  intervals: AggregationInterval[];
  omittedIntervals: number;
  failures: ServerFailure[];
}
export interface Boundary {
  atMs: number;
  down: Record<string, number>;
  up: Record<string, ReceiverCheckpoint | null>;
}
interface OpenInterval {
  record: AggregationInterval;
  first: Boundary | null;
  last: Boundary | null;
  stable: Boundary | null;
  wasStable: boolean;
  rates: Record<FlowDirection, FixedRateBuckets>;
  combined: FixedRateBuckets;
  peak: Record<FlowDirection, number>;
  score: number;
}
function directions(stage: TransferStage): FlowDirection[] {
  return stage === "bidirectional"
    ? ["down", "up"]
    : [stage === "download" ? "down" : "up"];
}
function sum(windows: ComponentWindow[] | null): number | null {
  return windows
    ? windows.reduce((total, window) => total + window.bytesPerSec, 0)
    : null;
}

/** Fixed membership and common boundary selection; receiver durations remain in their own clock domains. */
export class AggregateMeasurements {
  intervals: AggregationInterval[] = [];
  omittedIntervals = 0;
  #open: OpenInterval | null = null;
  #closed = new Map<number, OpenInterval>();
  #totals = new Map<string, Record<FlowDirection, number>>();
  #stageTotals = new Map<
    TransferStage,
    Map<string, Record<FlowDirection, number>>
  >();
  #stage: TransferStage | null = null;
  #uploadLedgers = new Map<
    string,
    Map<string, { baseline: number; maximum: number }>
  >();

  reset(): void {
    this.intervals = [];
    this.omittedIntervals = 0;
    this.#open = null;
    this.#closed.clear();
    this.#totals.clear();
    this.#stageTotals.clear();
    this.#stage = null;
    this.#uploadLedgers.clear();
  }
  begin(
    stage: TransferStage,
    participants: string[],
    atMs: number,
    reason: AggregationInterval["reason"] = "stage-start",
  ): void {
    this.close();
    this.#stage = stage;
    if (reason === "stage-start") this.#uploadLedgers.clear();
    if (this.intervals.length >= 128) {
      const discarded = this.intervals.shift()!;
      this.#closed.delete(discarded.id);
      this.omittedIntervals++;
    }
    const record: AggregationInterval = {
      id: this.omittedIntervals + this.intervals.length,
      stage,
      participants: [...participants],
      startMs: atMs,
      endMs: atMs,
      complete: true,
      reason,
      full: null,
      headline: null,
    };
    this.intervals.push(record);
    this.#open = {
      record,
      first: null,
      last: null,
      stable: null,
      wasStable: false,
      rates: {
        down: new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
        up: new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
      },
      combined: new FixedRateBuckets(TRANSFER_CONFIDENCE_BUCKETS),
      peak: { down: 0, up: 0 },
      score: 0,
    };
  }
  close(): void {
    if (this.#open) this.#closed.set(this.#open.record.id, this.#open);
    this.#open = null;
  }
  get current(): AggregationInterval | null {
    return this.#open?.record ?? null;
  }
  addDownload(serverId: string, bytes: number): void {
    if (Number.isFinite(bytes) && bytes > 0)
      this.#credit(serverId, "down", bytes);
  }
  #credit(id: string, dir: FlowDirection, bytes: number): void {
    this.#total(id)[dir] += bytes;
    if (!this.#stage) return;
    let stage = this.#stageTotals.get(this.#stage);
    if (!stage) {
      stage = new Map();
      this.#stageTotals.set(this.#stage, stage);
    }
    const total = stage.get(id) ?? { down: 0, up: 0 };
    total[dir] += bytes;
    stage.set(id, total);
  }
  stageTotals(stage: TransferStage, id: string): Record<FlowDirection, number> {
    return { ...(this.#stageTotals.get(stage)?.get(id) ?? { down: 0, up: 0 }) };
  }
  #total(id: string): Record<FlowDirection, number> {
    let total = this.#totals.get(id);
    if (!total) {
      total = { down: 0, up: 0 };
      this.#totals.set(id, total);
    }
    return total;
  }
  totals(id: string): Record<FlowDirection, number> {
    return { ...this.#total(id) };
  }
  beginUpload(id: string, checkpoint: ReceiverCheckpoint): void {
    let ledgers = this.#uploadLedgers.get(id);
    if (!ledgers) {
      ledgers = new Map();
      this.#uploadLedgers.set(id, ledgers);
    }
    if (!ledgers.has(checkpoint.id))
      ledgers.set(checkpoint.id, {
        baseline: checkpoint.bytes,
        maximum: checkpoint.bytes,
      });
  }
  observeUpload(
    id: string,
    checkpoint: Pick<ReceiverCheckpoint, "id" | "bytes">,
  ): void {
    const ledgers = this.#uploadLedgers.get(id);
    if (
      !ledgers ||
      !Number.isSafeInteger(checkpoint.bytes) ||
      checkpoint.bytes < 0
    )
      return;
    // Replacement receivers start during measurement. Their bytes are new, even without a preceding rate baseline.
    if (!ledgers.has(checkpoint.id))
      ledgers.set(checkpoint.id, { baseline: 0, maximum: 0 });
    const ledger = ledgers.get(checkpoint.id)!;
    if (checkpoint.bytes > ledger.maximum) {
      this.#credit(id, "up", checkpoint.bytes - ledger.maximum);
      ledger.maximum = checkpoint.bytes;
    }
  }
  observe(boundary: Boundary): AggregateWindow | null {
    const open = this.#open;
    if (!open) return null;
    const { record } = open;
    const valid =
      record.participants.length > 0 &&
      record.participants.every((id) =>
        directions(record.stage).every((dir) =>
          dir === "down"
            ? Number.isFinite(boundary.down[id])
            : boundary.up[id] !== null && boundary.up[id] !== undefined,
        ),
      );
    for (const id of record.participants)
      if (boundary.up[id]) {
        if (!this.#uploadLedgers.has(id))
          this.beginUpload(id, boundary.up[id]!);
        this.observeUpload(id, boundary.up[id]!);
      }
    if (!valid) {
      record.complete = false;
      record.endMs = boundary.atMs;
      open.wasStable = false;
      open.stable = null;
      return null;
    }
    if (!record.complete) {
      const stage = record.stage,
        participants = record.participants;
      this.begin(stage, participants, boundary.atMs, "evidence-resumed");
      return this.observe(boundary);
    }
    if (!open.first) {
      open.first = open.last = boundary;
      record.startMs = record.endMs = boundary.atMs;
      return null;
    }
    // A final flush can share the preceding browser clock tick or receiver snapshot.
    // It adds no duration; retain the last valid window without inventing a gap.
    const last = open.last!;
    const continuous = record.participants.every((id) =>
      directions(record.stage).every((dir) => {
        if (dir === "down") return boundary.down[id] >= last.down[id];
        const a = last.up[id],
          b = boundary.up[id];
        return (
          !!a &&
          !!b &&
          a.id === b.id &&
          b.bytes >= a.bytes &&
          (b.nanos > a.nanos || (b.nanos === a.nanos && b.bytes === a.bytes))
        );
      }),
    );
    if (
      continuous &&
      (boundary.atMs === last.atMs ||
        (boundary.atMs > last.atMs &&
          directions(record.stage).includes("up") &&
          record.participants.some(
            (id) => boundary.up[id]!.nanos === last.up[id]!.nanos,
          )))
    )
      return null;
    const sample = this.#window(open.last!, boundary, record);
    const full = this.#window(open.first, boundary, record);
    if (!sample || !full) {
      record.complete = false;
      record.endMs = boundary.atMs;
      this.begin(
        record.stage,
        record.participants,
        boundary.atMs,
        "evidence-resumed",
      );
      return this.observe(boundary);
    }
    const durationMs = boundary.atMs - open.last!.atMs;
    for (const dir of directions(record.stage)) {
      const rate =
        dir === "down" ? sample.downBytesPerSec! : sample.upBytesPerSec!;
      open.rates[dir].observe((rate * durationMs) / 1000, durationMs);
      open.peak[dir] = Math.max(open.peak[dir], rate);
    }
    open.combined.observe(
      (((sample.downBytesPerSec ?? 0) + (sample.upBytesPerSec ?? 0)) *
        durationMs) /
        1000,
      durationMs,
    );
    open.last = boundary;
    record.full = full;
    record.endMs = boundary.atMs;
    record.headline = open.stable
      ? this.#window(open.stable, boundary, record)
      : full;
    return sample;
  }
  #window(
    first: Boundary,
    last: Boundary,
    interval: AggregationInterval,
  ): AggregateWindow | null {
    if (last.atMs <= first.atMs) return null;
    let down: ComponentWindow[] | null = null,
      up: ComponentWindow[] | null = null;
    for (const dir of directions(interval.stage)) {
      const components: ComponentWindow[] = [];
      for (const serverId of interval.participants) {
        if (dir === "down") {
          const startBytes = first.down[serverId],
            endBytes = last.down[serverId],
            durationMs = last.atMs - first.atMs;
          if (
            !Number.isFinite(startBytes) ||
            !Number.isFinite(endBytes) ||
            endBytes < startBytes
          )
            return null;
          components.push({
            serverId,
            startBytes,
            endBytes,
            bytes: endBytes - startBytes,
            durationMs,
            bytesPerSec: ((endBytes - startBytes) * 1000) / durationMs,
            clock: "client-monotonic",
          });
        } else {
          const a = first.up[serverId],
            b = last.up[serverId];
          if (
            !a ||
            !b ||
            a.id !== b.id ||
            b.bytes < a.bytes ||
            b.nanos <= a.nanos
          )
            return null;
          const durationMs = (b.nanos - a.nanos) / 1e6;
          components.push({
            serverId,
            startBytes: a.bytes,
            endBytes: b.bytes,
            bytes: b.bytes - a.bytes,
            durationMs,
            bytesPerSec: ((b.bytes - a.bytes) * 1000) / durationMs,
            clock: "receiver",
            startNanos: a.nanos,
            endNanos: b.nanos,
            startRequestMs: a.requestedAtMs,
            startResponseMs: a.receivedAtMs,
            endRequestMs: b.requestedAtMs,
            endResponseMs: b.receivedAtMs,
          });
        }
      }
      if (dir === "down") down = components;
      else up = components;
    }
    return {
      startMs: first.atMs,
      endMs: last.atMs,
      down,
      up,
      downBytesPerSec: sum(down),
      upBytesPerSec: sum(up),
    };
  }
  confidence(): ConfidenceScore {
    return transferConfidence([...(this.#open?.combined.rates ?? [])]);
  }
  trackStable(score: number, cfg: RunnerConfig["adaptive"]): boolean {
    const open = this.#open;
    if (!open || !open.record.complete) return false;
    const stable = isStillStable(open.wasStable, score, cfg);
    if (stable && !open.wasStable) open.stable = open.last;
    if (!stable) open.stable = null;
    open.wasStable = stable;
    open.score = score;
    return stable;
  }
  result(
    stage: TransferStage,
    dir: FlowDirection,
    stable: boolean,
  ): ThroughputResult | null {
    const record = this.intervals.findLast(
      (interval) => interval.stage === stage,
    );
    if (
      !record ||
      !record.complete ||
      !record.full ||
      record.endMs - record.startMs < MIN_PARTIAL_TRANSFER_EVIDENCE_MS
    )
      return null;
    const open =
      this.#open?.record === record ? this.#open : this.#closed.get(record.id);
    if (!open) return null;
    const window =
      stable &&
      record.headline &&
      record.headline.endMs - record.headline.startMs >=
        MIN_PARTIAL_TRANSFER_EVIDENCE_MS
        ? record.headline
        : record.full;
    const components = window[dir];
    if (
      !components ||
      components.some(
        (component) => component.durationMs < MIN_PARTIAL_TRANSFER_EVIDENCE_MS,
      )
    )
      return null;
    const key = dir === "down" ? "downBytesPerSec" : "upBytesPerSec";
    const rate = window[key],
      full = record.full[key];
    if (rate === null || full === null) return null;
    record.headline = window;
    const confidence = transferConfidence([...open.rates[dir].rates]);
    return {
      reportedBytesPerSec: rate,
      fullAverageBytesPerSec: full,
      totalBytes: [...(this.#stageTotals.get(stage)?.values() ?? [])].reduce(
        (total, bytes) => total + bytes[dir],
        0,
      ),
      peakBytesPerSec: open.peak[dir],
      stabilityPct:
        confidence.sampleCount >= 2
          ? Math.max(0, 1 - confidence.varianceRatio) * 100
          : 0,
      method: window !== record.full ? "stable-window" : "full-average",
      stabilityScore: open.score,
      band: bandForState(open.wasStable, open.score),
      probeTimeoutPct: null,
      serverAuthoritative: dir === "up" || undefined,
    };
  }
}

/** Completion respects the least stable and least sampled paths without combining their RTT populations. */
export function weakestLatencyConfidence(
  accumulators: readonly Pick<RunAccumulator, "confidence">[],
): LatencyConfidenceScore {
  if (!accumulators.length)
    return { score: 0, sampleCount: 0, jitterRatio: 0, lossRatio: 0 };
  return accumulators
    .map((accum) => accum.confidence("latency") as LatencyConfidenceScore)
    .reduce(
      (weakest, next) => ({
        score: Math.min(weakest.score, next.score),
        sampleCount: Math.min(weakest.sampleCount, next.sampleCount),
        jitterRatio: Math.max(weakest.jitterRatio, next.jitterRatio),
        lossRatio: Math.max(weakest.lossRatio, next.lossRatio),
      }),
      { score: 1, sampleCount: Infinity, jitterRatio: 0, lossRatio: 0 },
    );
}
