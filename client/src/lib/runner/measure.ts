import type {
  AdaptiveDurationConfig,
  BufferbloatGrade,
  FlowDirection,
  LatencyObservation,
  LatencyResult,
  PingCadence,
  PreparedPaths,
  ReceiverCheckpoint,
  RunnerConfig,
  StabilityBand,
  StageLatencySummary,
  ThroughputResult,
  TransportRole,
} from "./contract";
import type { ServerIdentity } from "../servers/catalog";
import { fixedPingIntervalMs } from "./pingCadence";

export const MIN_EVIDENCE_MS = 800;
const MIN_PARTIAL_LATENCY_OUTCOMES = 3;
export const BUCKET_MS = 250;
const WINDOW_MS = 4_000;
const WINDOW_BUCKETS = WINDOW_MS / BUCKET_MS;
const INTERVAL_LIMIT = 128;
export const STAGES = [
  "latency",
  "download",
  "upload",
  "bidirectional",
] as const;
export type TransferStage = Exclude<TransportRole, "latency">;

export function median(xs: readonly number[]): number {
  if (!xs.length) return 0;
  const s = xs.toSorted((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}

/** Nearest rank; 0 for empty. */
export function percentile(xs: readonly number[], p: number): number {
  if (!xs.length) return 0;
  const s = xs.toSorted((a, b) => a - b);
  return s[
    Math.min(s.length - 1, Math.max(0, Math.ceil((p / 100) * s.length) - 1))
  ];
}

const mean = (xs: readonly number[]) =>
  xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0;
const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/** Exact byte/time evidence split into fixed-duration rate buckets. */
export class RateBuckets {
  #bytes = 0;
  #ms = 0;
  readonly rates: number[] = [];
  constructor(readonly limit = Infinity) {}

  observe(bytes: number, ms: number): void {
    if (!(ms > 0) || !Number.isFinite(ms)) return;
    bytes = Number.isFinite(bytes) ? Math.max(0, bytes) : 0;
    while (ms > 0) {
      const take = Math.min(BUCKET_MS - this.#ms, ms);
      const part = (bytes * take) / ms;
      this.#bytes += part;
      this.#ms += take;
      bytes -= part;
      ms -= take;
      if (this.#ms < BUCKET_MS - 1e-9) continue;
      this.rates.push((this.#bytes * 1000) / BUCKET_MS);
      if (this.rates.length > this.limit) this.rates.shift();
      this.#bytes = this.#ms = 0;
    }
  }
}

export interface ConfidenceScore {
  score: number;
  sampleCount: number;
  varianceRatio?: number;
  slopeRatio?: number;
  jitterRatio?: number;
  lossRatio?: number;
}

/** score = 1 − 2.2·CV − 1.4·|first third − last third| / mean over the trailing window. */
export function transferConfidence(rates: readonly number[]): ConfidenceScore {
  const values = rates.slice(-WINDOW_BUCKETS);
  const avg = mean(values);
  if (values.length < 2 || avg <= 0)
    return {
      score: 0,
      varianceRatio: 1,
      slopeRatio: 1,
      sampleCount: values.length,
    };
  const varianceRatio =
    Math.sqrt(mean(values.map((v) => (v - avg) ** 2))) / avg;
  const segment = Math.max(2, Math.ceil(values.length / 3));
  const slopeRatio =
    Math.abs(mean(values.slice(-segment)) - mean(values.slice(0, segment))) /
    avg;
  const score = clamp01(1 - varianceRatio * 2.2 - slopeRatio * 1.4);
  return { score, varianceRatio, slopeRatio, sampleCount: values.length };
}

/** Descriptive 0..100 steadiness of fixed-time rate buckets. */
export function stabilityPct(rates: readonly number[]): number {
  const { sampleCount, varianceRatio = 1 } = transferConfidence(rates);
  return sampleCount >= 2 ? Math.max(0, 1 - varianceRatio) * 100 : 0;
}

/** score = 1 − 1.2·(median deviation / max(median, 20 ms)) − 3.6·timeout ratio. */
export function latencyConfidence(
  outcomes: readonly { t: number; rtt: number | null }[],
): ConfidenceScore {
  const latest = outcomes.at(-1)?.t ?? 0;
  const window = outcomes.filter((outcome) => outcome.t > latest - WINDOW_MS);
  const values = window.flatMap((outcome) =>
    outcome.rtt == null ? [] : [outcome.rtt],
  );
  if (values.length < 2)
    return {
      score: 0,
      jitterRatio: 1,
      lossRatio: 1,
      sampleCount: window.length,
    };
  const center = median(values);
  const jitterRatio =
    median(values.map((v) => Math.abs(v - center))) / Math.max(center, 20);
  const lossRatio = (window.length - values.length) / window.length;
  const score = clamp01(1 - jitterRatio * 1.2 - lossRatio * 3.6);
  return { score, jitterRatio, lossRatio, sampleCount: window.length };
}

/** Schmitt trigger: enter at the threshold, leave 0.08 below it. */
export function isStillStable(
  wasStable: boolean,
  score: number,
  cfg: AdaptiveDurationConfig,
): boolean {
  return score >= cfg.stabilityThreshold - (wasStable ? 0.08 : 0);
}

export function bandForState(stable: boolean, score: number): StabilityBand {
  return stable ? "high" : score >= 0.6 ? "medium" : "low";
}

/** The evidence floor a phase can feasibly reach within its budget; never below the statistical minimum. */
export function confidenceSampleFloor(
  kind: "latency" | "transfer",
  durationMs: number,
  cfg: AdaptiveDurationConfig,
  cadence?: PingCadence,
): number {
  const finite = (value: number) =>
    Number.isFinite(value) ? Math.max(0, value) : 0;
  const requested = Math.floor(
    finite(kind === "latency" ? cfg.minLatencySamples : cfg.minTransferSamples),
  );
  if (!requested) return 0;
  const budgetMs = Math.max(0, finite(durationMs) - finite(cfg.confirmationMs));
  const intervalMs =
    kind === "latency" ? cadence && fixedPingIntervalMs(cadence) : BUCKET_MS;
  if (!intervalMs) return requested;
  const capacity =
    kind === "latency"
      ? Math.min(
          Math.ceil(WINDOW_MS / intervalMs),
          1 + Math.floor(budgetMs / intervalMs),
        )
      : Math.min(WINDOW_BUCKETS, Math.floor(budgetMs / intervalMs));
  const minimum = Math.min(requested, kind === "latency" ? 3 : 4);
  return Math.max(minimum, Math.min(requested, capacity));
}

/** An early exit needs coverage, a stable score and enough evidence. */
export function shouldExitPhase(input: {
  kind: "latency" | "transfer";
  cadence?: PingCadence;
  elapsedMs: number;
  durationMs: number;
  confidence: ConfidenceScore;
  cfg: AdaptiveDurationConfig;
}): boolean {
  const { cfg, durationMs, confidence } = input;
  return (
    cfg.enabled &&
    durationMs > 0 &&
    input.elapsedMs / durationMs >=
      Math.max(cfg.minCoverageRatio, 1 - cfg.maxPhaseReductionRatio) &&
    confidence.score >= cfg.stabilityThreshold &&
    confidence.sampleCount >=
      confidenceSampleFloor(input.kind, durationMs, cfg, input.cadence)
  );
}

/** Raw outcomes of one stage; presentation buckets never feed it. */
export class LatencyPopulation {
  readonly rtts: number[] = [];
  #timeouts = 0;
  #replies = 0;
  #unresolved = 0;
  #sendFailures = 0;
  #timing = { count: 0, raw: 0, handling: 0 };
  #deltaSum = 0;
  #deltaCount = 0;
  #previous: number | null = null;
  #continuity = 0;
  #complete = true;

  get count(): number {
    return this.#replies + this.#timeouts;
  }
  get timeoutPct(): number | null {
    return this.count ? (100 * this.#timeouts) / this.count : null;
  }

  observe(sample: LatencyObservation, continuity = 0): void {
    if (continuity !== this.#continuity) this.#previous = null;
    this.#continuity = continuity;
    const { rttMs, reflectorHandlingMs: handling } = sample;
    const valid = !sample.lost && Number.isFinite(rttMs) && rttMs >= 0;
    if (sample.lost) this.#timeouts++;
    else if (valid) this.#replies++;
    if (!valid || sample.rttEligible === false) return;
    this.rtts.push(rttMs);
    if (
      handling !== undefined &&
      Number.isFinite(handling) &&
      handling >= 0 &&
      handling <= rttMs
    ) {
      this.#timing.count++;
      this.#timing.raw += rttMs;
      this.#timing.handling += handling;
    }
    if (this.#previous !== null) {
      this.#deltaSum += Math.abs(rttMs - this.#previous);
      this.#deltaCount++;
    }
    this.#previous = rttMs;
  }

  interrupt(count: number, reason: "unresolved" | "send-failed"): void {
    if (!Number.isSafeInteger(count) || count <= 0) return;
    if (reason === "unresolved") this.#unresolved += count;
    else this.#sendFailures += count;
    this.#previous = null;
  }

  markIncomplete(): void {
    this.#complete = false;
    this.#previous = null;
  }

  summary(): StageLatencySummary | null {
    if (
      !this.count &&
      !this.#unresolved &&
      !this.#sendFailures &&
      this.#complete
    )
      return null;
    const sorted = this.rtts.toSorted((a, b) => a - b);
    const rank = (p: number) =>
      sorted.length
        ? sorted[Math.max(0, Math.ceil(p * sorted.length) - 1)]
        : null;
    const { count, raw, handling } = this.#timing;
    return {
      ...(count
        ? {
            reflectorTiming: {
              sampleCount: count,
              meanRawRttMs: raw / count,
              meanHandlingMs: handling / count,
              meanAdjustedRttMs: (raw - handling) / count,
            },
          }
        : {}),
      accountingComplete: this.#complete,
      probeCount: this.count,
      timeoutCount: this.#timeouts,
      unresolvedCount: this.#unresolved,
      sendFailureCount: this.#sendFailures,
      jitterPairs: this.#deltaCount,
      minMs: sorted[0] ?? null,
      maxMs: sorted.at(-1) ?? null,
      meanMs: sorted.length ? mean(this.rtts) : null,
      p10Ms: rank(0.1),
      p50Ms: sorted.length ? median(sorted) : null,
      p90Ms: rank(0.9),
      p95Ms: rank(0.95),
      jitterMs: this.#deltaCount ? this.#deltaSum / this.#deltaCount : null,
    };
  }
}

/** One server's latency populations and the idle stage's adaptive window. */
export class ServerLatency {
  readonly stages = {
    latency: new LatencyPopulation(),
    download: new LatencyPopulation(),
    upload: new LatencyPopulation(),
    bidirectional: new LatencyPopulation(),
  };
  readonly failed = new Set<TransportRole>();
  #window: { t: number; rtt: number | null }[] = [];
  #head = 0;
  #stableStart = -1;
  #candidate = -1;
  #earlyStart = -1;
  #score = 0;

  observe(
    stage: TransportRole,
    sample: LatencyObservation,
    t: number,
    continuity: number,
  ): void {
    this.stages[stage].observe(sample, continuity);
    if (stage !== "latency" || sample.rttEligible === false) return;
    this.#window.push({ t, rtt: sample.lost ? null : sample.rttMs });
    while (this.#window[this.#head]?.t <= t - WINDOW_MS) this.#head++;
    // Amortized trimming keeps dense reply-driven windows linear.
    if (this.#head < 4096) return;
    this.#window.splice(0, this.#head);
    this.#head = 0;
  }

  confidence(): ConfidenceScore {
    return latencyConfidence(this.#window.slice(this.#head));
  }

  resetStability(): void {
    this.#window = [];
    this.#head = 0;
    this.#stableStart = -1;
  }

  trackStable(score: number, cfg: AdaptiveDurationConfig): boolean {
    const stable = isStillStable(this.#stableStart >= 0, score, cfg);
    this.#score = score;
    if (!stable) this.#stableStart = -1;
    else if (this.#stableStart < 0)
      this.#stableStart = Math.max(0, this.stages.latency.rtts.length - 1);
    return stable;
  }

  earlyStop(action: "arm" | "cancel" | "confirm"): void {
    const at = Math.max(0, this.stages.latency.rtts.length - 1);
    if (action === "arm" && this.#candidate < 0) this.#candidate = at;
    if (action === "confirm" && this.#candidate >= 0)
      this.#earlyStart = this.#candidate;
    if (action !== "arm") this.#candidate = -1;
  }

  /** The idle headline; a failed population needs three outcomes and never uses a stable window. */
  result(config: RunnerConfig): LatencyResult | null {
    const idle = this.stages.latency;
    const summary = idle.summary();
    const failed = this.failed.has("latency");
    if (
      !summary ||
      !idle.rtts.length ||
      (failed && idle.count < MIN_PARTIAL_LATENCY_OUTCOMES)
    )
      return null;
    const length = idle.rtts.length;
    let start =
      config.adaptive.enabled && !failed && this.#stableStart < length
        ? this.#stableStart
        : -1;
    if (start >= 0 && this.#earlyStart >= 0 && this.#earlyStart < length)
      start = start <= this.#earlyStart ? this.#earlyStart : -1;
    const idleMs = start >= 0 ? median(idle.rtts.slice(start)) : summary.p50Ms!;
    return {
      idleMs,
      reportedMs: idleMs,
      minMs: summary.minMs,
      p50Ms: summary.p50Ms,
      p95Ms: summary.p95Ms,
      jitterMs: summary.jitterMs,
      probeTimeoutPct: idle.timeoutPct,
      method: start >= 0 ? "stable-window" : "full-average",
      stabilityScore: this.#score,
      band: bandForState(this.#stableStart >= 0, this.#score),
    };
  }

  summaries(): Record<TransportRole, StageLatencySummary | null> {
    const s = this.stages;
    return {
      latency: s.latency.summary(),
      download: s.download.summary(),
      upload: s.upload.summary(),
      bidirectional: s.bidirectional.summary(),
    };
  }

  /** The worst loaded median against the full idle median; a negative difference stays negative. */
  bufferbloat(): BufferbloatGrade | null {
    const loaded = (["download", "upload", "bidirectional"] as const).flatMap(
      (stage) => {
        const p50 = this.stages[stage].summary()?.p50Ms;
        return p50 == null ? [] : [p50];
      },
    );
    if (!this.stages.latency.rtts.length || !loaded.length) return null;
    const idleMs = median(this.stages.latency.rtts);
    const loadedMs = Math.max(...loaded);
    const increaseMs = loadedMs - idleMs;
    const grade =
      increaseMs <= 5
        ? "A"
        : increaseMs <= 30
          ? "B"
          : increaseMs <= 60
            ? "C"
            : increaseMs <= 200
              ? "D"
              : "F";
    return { grade, idleMs, loadedMs, increaseMs };
  }
}

/** One presentation of a stage population, shared by live results and history. */
export interface LatencyLaneSnapshot {
  reflectorTiming?: StageLatencySummary["reflectorTiming"];
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  p95?: number | null;
  center: number | null;
  jitter: number | null;
  timeoutRatio: number | null;
  accountingComplete: boolean;
  timeoutCount: number;
  unresolvedCount: number;
  sendFailureCount: number;
  count: number;
}

export function latencyLanes(
  summaries: Partial<Record<TransportRole, StageLatencySummary | null>>,
): Record<TransportRole, LatencyLaneSnapshot | null> {
  const lane = (stage: TransportRole): LatencyLaneSnapshot | null => {
    const s = summaries[stage];
    if (!s) return null;
    return {
      ...(s.reflectorTiming
        ? { reflectorTiming: { ...s.reflectorTiming } }
        : {}),
      min: s.minMs,
      max: s.maxMs,
      p10: s.p10Ms,
      p90: s.p90Ms,
      p95: s.p95Ms,
      center: s.p50Ms,
      jitter: s.jitterMs,
      timeoutRatio: s.probeCount ? s.timeoutCount / s.probeCount : null,
      accountingComplete: s.accountingComplete,
      timeoutCount: s.timeoutCount,
      unresolvedCount: s.unresolvedCount,
      sendFailureCount: s.sendFailureCount,
      count: s.probeCount,
    };
  };
  return {
    latency: lane("latency"),
    download: lane("download"),
    upload: lane("upload"),
    bidirectional: lane("bidirectional"),
  };
}

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
/** The verified paths a server's results were measured on. */
export function pathEvidence(
  paths: PreparedPaths,
): Pick<ServerMeasurementSummary, "throughput" | "latencyTarget"> {
  const { throughput, latency } = paths;
  return {
    throughput: {
      origin: throughput.target.origin,
      transport: throughput.target.transport,
      protocol: throughput.fetch.protocol,
      ...(throughput.browserProtocol
        ? { browserProtocol: throughput.browserProtocol }
        : {}),
      clientIpVersion: throughput.probe.clientIpVersion,
    },
    latencyTarget: latency
      ? { origin: latency.target.origin, transport: latency.target.transport }
      : null,
  };
}

export interface Boundary {
  atMs: number;
  down: Record<string, number>;
  up: Record<string, ReceiverCheckpoint | null>;
}
interface Series {
  rates: Record<FlowDirection, RateBuckets>;
  peak: Record<FlowDirection, number>;
}
interface OpenInterval {
  record: AggregationInterval;
  first: Boundary | null;
  last: Boundary | null;
  stable: Boundary | null;
  wasStable: boolean;
  score: number;
  combined: RateBuckets;
  total: Series;
  servers: Map<string, Series>;
}
type Totals = Record<FlowDirection, number>;

const series = (): Series => ({
  rates: { down: new RateBuckets(), up: new RateBuckets() },
  peak: { down: 0, up: 0 },
});
const directions = (stage: TransferStage): FlowDirection[] =>
  stage === "bidirectional"
    ? ["down", "up"]
    : [stage === "download" ? "down" : "up"];
const rateOf = (window: AggregateWindow, dir: FlowDirection) =>
  dir === "down" ? window.downBytesPerSec : window.upBytesPerSec;

/** Fixed-membership intervals over common boundaries; each receiver keeps its own clock. */
export class ThroughputAggregate {
  intervals: AggregationInterval[] = [];
  omittedIntervals = 0;
  #open: OpenInterval | null = null;
  #closed = new Map<number, OpenInterval>();
  #stageTotals = new Map<TransferStage, Map<string, Totals>>();
  #ledgers = new Map<string, Map<string, number>>();

  get current(): AggregationInterval | null {
    return this.#open?.record ?? null;
  }

  begin(
    stage: TransferStage,
    participants: string[],
    atMs: number,
    reason: AggregationInterval["reason"] = "stage-start",
  ): void {
    this.close();
    if (reason === "stage-start") this.#ledgers.clear();
    if (this.intervals.length >= INTERVAL_LIMIT) {
      this.#closed.delete(this.intervals.shift()!.id);
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
      score: 0,
      combined: new RateBuckets(WINDOW_BUCKETS),
      total: series(),
      servers: new Map(participants.map((id) => [id, series()])),
    };
  }

  close(): void {
    if (this.#open) this.#closed.set(this.#open.record.id, this.#open);
    this.#open = null;
  }

  /** Unique measured bytes per server; overlapping evidence is never counted twice. */
  totals(id: string): Totals {
    const sum = { down: 0, up: 0 };
    for (const stage of this.#stageTotals.values())
      for (const dir of ["down", "up"] as const)
        sum[dir] += stage.get(id)?.[dir] ?? 0;
    return sum;
  }

  #credit(
    stage: TransferStage,
    id: string,
    dir: FlowDirection,
    bytes: number,
  ): void {
    if (!(bytes > 0)) return;
    const totals = this.#stageTotals.get(stage) ?? new Map<string, Totals>();
    this.#stageTotals.set(stage, totals);
    const total = totals.get(id) ?? { down: 0, up: 0 };
    total[dir] += bytes;
    totals.set(id, total);
  }

  /** Download bytes are credited as they are consumed; upload bytes from receiver maxima. */
  addDownload(stage: TransferStage, id: string, bytes: number): void {
    this.#credit(stage, id, "down", bytes);
  }

  #observeUpload(
    stage: TransferStage,
    id: string,
    checkpoint: ReceiverCheckpoint,
  ): void {
    const ledgers = this.#ledgers.get(id) ?? new Map<string, number>();
    this.#ledgers.set(id, ledgers);
    // A receiver first seen during a stage starts at its current count; a replacement starts at zero.
    const maximum =
      ledgers.get(checkpoint.id) ?? (ledgers.size ? 0 : checkpoint.bytes);
    if (checkpoint.bytes > maximum)
      this.#credit(stage, id, "up", checkpoint.bytes - maximum);
    ledgers.set(checkpoint.id, Math.max(maximum, checkpoint.bytes));
  }

  /** A boundary missing any component is skipped; the next valid one spans the gap. */
  observe(boundary: Boundary): AggregateWindow | null {
    const open = this.#open;
    if (!open) return null;
    const { record } = open;
    const dirs = directions(record.stage);
    for (const id of record.participants) {
      const up = boundary.up[id];
      if (up && Number.isSafeInteger(up.bytes) && up.bytes >= 0)
        this.#observeUpload(record.stage, id, up);
    }
    const valid =
      record.participants.length > 0 &&
      record.participants.every((id) =>
        dirs.every((dir) =>
          dir === "down"
            ? Number.isFinite(boundary.down[id])
            : !!boundary.up[id],
        ),
      );
    if (!valid) return null;
    if (!open.first || !open.last) {
      open.first = open.last = boundary;
      record.startMs = record.endMs = boundary.atMs;
      return null;
    }
    const last = open.last;
    const continuous = record.participants.every((id) =>
      dirs.every((dir) => {
        if (dir === "down") return boundary.down[id] >= last.down[id];
        const [a, b] = [last.up[id]!, boundary.up[id]!];
        return (
          a.id === b.id &&
          b.bytes >= a.bytes &&
          (b.nanos > a.nanos || (b.nanos === a.nanos && b.bytes === a.bytes))
        );
      }),
    );
    // A final flush in the same tick, or an unchanged receiver clock, adds no window.
    const unchanged =
      boundary.atMs <= last.atMs ||
      (dirs.includes("up") &&
        record.participants.some(
          (id) => boundary.up[id]!.nanos === last.up[id]!.nanos,
        ));
    if (continuous && unchanged) return null;
    const sample = window(last, boundary, record);
    const full = window(open.first, boundary, record);
    if (!sample || !full) {
      // A replaced receiver or regressed counter cannot be spanned.
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
    const ms = boundary.atMs - last.atMs;
    for (const dir of dirs) {
      const rate = rateOf(sample, dir)!;
      open.total.rates[dir].observe((rate * ms) / 1000, ms);
      open.total.peak[dir] = Math.max(open.total.peak[dir], rate);
      for (const component of sample[dir]!) {
        const server = open.servers.get(component.serverId)!;
        server.rates[dir].observe(component.bytes, component.durationMs);
        server.peak[dir] = Math.max(server.peak[dir], component.bytesPerSec);
      }
    }
    open.combined.observe(
      (((sample.downBytesPerSec ?? 0) + (sample.upBytesPerSec ?? 0)) * ms) /
        1000,
      ms,
    );
    open.last = boundary;
    record.full = full;
    record.endMs = boundary.atMs;
    record.headline = open.stable
      ? window(open.stable, boundary, record)
      : full;
    return sample;
  }

  confidence(): ConfidenceScore {
    return transferConfidence(this.#open?.combined.rates ?? []);
  }

  resetStability(): void {
    const open = this.#open;
    if (!open) return;
    open.combined = new RateBuckets(WINDOW_BUCKETS);
    open.wasStable = false;
    open.stable = null;
  }

  trackStable(score: number, cfg: AdaptiveDurationConfig): boolean {
    const open = this.#open;
    if (!open || !open.record.complete) return false;
    const stable = isStillStable(open.wasStable, score, cfg);
    open.stable = stable ? (open.wasStable ? open.stable : open.last) : null;
    open.wasStable = stable;
    open.score = score;
    return stable;
  }

  #interval(record: AggregationInterval): OpenInterval | undefined {
    return this.#open?.record === record
      ? this.#open
      : this.#closed.get(record.id);
  }

  #stageTotal(stage: TransferStage, dir: FlowDirection, id?: string): number {
    let sum = 0;
    for (const [server, totals] of this.#stageTotals.get(stage) ?? [])
      if (!id || server === id) sum += totals[dir];
    return sum;
  }

  /** The stage headline from its latest interval; saved evidence names the reported window. */
  result(
    stage: TransferStage,
    stable: boolean,
  ): Record<FlowDirection, ThroughputResult | null> {
    const record = this.intervals.findLast(
      (interval) => interval.stage === stage,
    );
    const open = record && this.#interval(record);
    const none = { down: null, up: null };
    if (!record?.complete || !record.full || !open) return none;
    const window =
      stable && sufficient(record.headline) ? record.headline! : record.full;
    record.headline = window;
    // An uninterrupted stage reports whatever it measured; a later interval needs the evidence floor.
    const whole = record.reason === "stage-start" && window === record.full;
    const reduce = (dir: FlowDirection): ThroughputResult | null => {
      const rate = rateOf(window, dir);
      if (rate === null || (!whole && !sufficient(window))) return null;
      return {
        reportedBytesPerSec: rate,
        fullAverageBytesPerSec: rateOf(record.full!, dir)!,
        totalBytes: this.#stageTotal(stage, dir),
        peakBytesPerSec: open.total.peak[dir],
        stabilityPct: stabilityPct(open.total.rates[dir].rates),
        method: window === record.full ? "full-average" : "stable-window",
        stabilityScore: open.score,
        band: bandForState(open.wasStable, open.score),
        serverAuthoritative: dir === "up" || undefined,
      };
    };
    return { down: reduce("down"), up: reduce("up") };
  }

  /** One server's share from the latest interval it took part in, including before a dropout. */
  serverResult(
    stage: TransferStage,
    dir: FlowDirection,
    id: string,
  ): ThroughputResult | null {
    const record = this.intervals.findLast(
      (interval) =>
        interval.stage === stage &&
        interval.full &&
        interval.participants.includes(id),
    );
    const component = record?.full?.[dir]?.find((c) => c.serverId === id);
    const open = record && this.#interval(record);
    if (!component || !open || component.durationMs < MIN_EVIDENCE_MS)
      return null;
    const server = open.servers.get(id)!;
    return {
      reportedBytesPerSec: component.bytesPerSec,
      fullAverageBytesPerSec: component.bytesPerSec,
      totalBytes: this.#stageTotal(stage, dir, id),
      peakBytesPerSec: server.peak[dir],
      stabilityPct: stabilityPct(server.rates[dir].rates),
      method: "full-average",
      stabilityScore: 0,
      band: "low",
      serverAuthoritative: dir === "up" || undefined,
    };
  }
}

/** A reportable window spans the evidence floor in the client clock and in every receiver clock. */
function sufficient(window: AggregateWindow | null): boolean {
  return (
    !!window &&
    window.endMs - window.startMs >= MIN_EVIDENCE_MS &&
    [...(window.down ?? []), ...(window.up ?? [])].every(
      (c) => c.durationMs >= MIN_EVIDENCE_MS,
    )
  );
}

function window(
  first: Boundary,
  last: Boundary,
  interval: AggregationInterval,
): AggregateWindow | null {
  if (last.atMs <= first.atMs) return null;
  const result: AggregateWindow = {
    startMs: first.atMs,
    endMs: last.atMs,
    down: null,
    up: null,
    downBytesPerSec: null,
    upBytesPerSec: null,
  };
  for (const dir of directions(interval.stage)) {
    const components: ComponentWindow[] = [];
    for (const serverId of interval.participants) {
      if (dir === "down") {
        const startBytes = first.down[serverId];
        const endBytes = last.down[serverId];
        const durationMs = last.atMs - first.atMs;
        if (!(endBytes >= startBytes)) return null;
        const bytes = endBytes - startBytes;
        components.push({
          serverId,
          startBytes,
          endBytes,
          bytes,
          durationMs,
          bytesPerSec: (bytes * 1000) / durationMs,
          clock: "client-monotonic",
        });
        continue;
      }
      const a = first.up[serverId];
      const b = last.up[serverId];
      if (!a || !b || a.id !== b.id || b.bytes < a.bytes || b.nanos <= a.nanos)
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
    const sum = components.reduce((total, c) => total + c.bytesPerSec, 0);
    if (dir === "down")
      [result.down, result.downBytesPerSec] = [components, sum];
    else [result.up, result.upBytesPerSec] = [components, sum];
  }
  return result;
}
