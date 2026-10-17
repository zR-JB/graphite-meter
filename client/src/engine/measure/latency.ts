// Latency statistics (docs/MEASUREMENTS.md#latency-probing): one population per server and stage, exact nearest-rank
// percentiles with a midpoint median, jitter over adjacent replies, and the RFC 6298 probe deadline.
import type { ProbeOutcome } from "../../protocol/messages";
import type { Stage, TransferStage } from "../run/schedule";
import { SCORE_WINDOW_MS, type Confidence } from "./stability";

/** A stage population's statistics; history saves them as they are. */
export interface LatencySummary {
  /** Means over in-window replies with a valid server handling time. */
  reflectorTiming?: { sampleCount: number; meanRawRttMs: number; meanHandlingMs: number };
  /** False when a lost worker leaves the population's outcomes unknown. */
  accountingComplete: boolean;
  probeCount: number;
  timeoutCount: number;
  unresolvedCount: number;
  sendFailureCount: number;
  jitterPairs: number;
  minMs: number | null;
  maxMs: number | null;
  meanMs: number | null;
  p10Ms: number | null;
  p50Ms: number | null;
  p90Ms: number | null;
  p95Ms: number | null;
  jitterMs: number | null;
}

type Outcome = Pick<ProbeOutcome, "rttMs" | "timedOut" | "handlingMs" | "inWindow">;

/** The 0-based nearest rank of percentile `p` among `n` sorted values. */
export const nearestRank = (p: number, n: number) => Math.min(n - 1, Math.max(0, Math.ceil(p * n) - 1));

/** The median of `n` sorted values read through `at`, the mean of the middle two for an even count. */
export const midpointMedian = (n: number, at: (rank: number) => number) =>
  n % 2 ? at(n >> 1) : (at((n >> 1) - 1) + at(n >> 1)) / 2;

/** Distinct RTTs kept to the nanosecond; past them new ones keep four significant digits (0.01 %), so a day-long
    stage holds tens of thousands of values at most. */
const EXACT_VALUES = 65_536;
const significant = (ns: number) => {
  if (ns < 10_000) return ns;
  const step = 10 ** (Math.floor(Math.log10(ns)) - 3);
  return Math.round(ns / step) * step;
};

/** One stage's outcomes. RTTs are kept once per distinct nanosecond value with a count, so a dense loopback run
    holds its few thousand distinct values rather than millions of samples. */
export class Population {
  #counts = new Map<number, number>();
  #sorted = new Float64Array(0);
  #fresh: number[] = [];
  #rtts = 0;
  #sum = 0;
  #replies = 0;
  #timeouts = 0;
  #unresolved = 0;
  #sendFailures = 0;
  #timing = { count: 0, raw: 0, handling: 0 };
  #jitterSum = 0;
  #jitterPairs = 0;
  #previous: number | null = null;
  #continuity = 0;
  #complete = true;
  #cached: LatencySummary | null | undefined;
  #final: LatencySummary | null | undefined;

  /** Resolved probes: replies and timeouts. */
  get count(): number {
    return this.#replies + this.#timeouts;
  }

  /** `continuity` changes when the channel reconnects, which breaks reply adjacency for jitter. */
  observe(outcome: Outcome, continuity = 0): void {
    this.#cached = undefined;
    if (continuity !== this.#continuity) this.#previous = null;
    this.#continuity = continuity;
    const { rttMs, handlingMs } = outcome;
    if (outcome.timedOut) {
      this.#timeouts++;
      return;
    }
    if (!(rttMs >= 0) || !Number.isFinite(rttMs)) return;
    this.#replies++;
    if (!outcome.inWindow) return;
    const exact = Math.round(rttMs * 1e6);
    const ns = this.#counts.size < EXACT_VALUES ? exact : significant(exact);
    const seen = this.#counts.get(ns);
    if (seen === undefined) this.#fresh.push(ns);
    this.#counts.set(ns, (seen ?? 0) + 1);
    this.#rtts++;
    this.#sum += rttMs;
    if (handlingMs != null && handlingMs >= 0 && handlingMs <= rttMs) {
      this.#timing.count++;
      this.#timing.raw += rttMs;
      this.#timing.handling += handlingMs;
    }
    if (this.#previous !== null) {
      this.#jitterSum += Math.abs(rttMs - this.#previous);
      this.#jitterPairs++;
    }
    this.#previous = rttMs;
  }

  /** Probes the channel lost before a verdict, or could not send; neither is a timeout. */
  interrupt(count: number, reason: "unresolved" | "send-failed"): void {
    if (!Number.isSafeInteger(count) || count <= 0) return;
    this.#cached = undefined;
    if (reason === "unresolved") this.#unresolved += count;
    else this.#sendFailures += count;
    this.#previous = null;
  }

  markIncomplete(): void {
    this.#cached = undefined;
    this.#complete = false;
    this.#previous = null;
  }

  /** Freezes the summary and frees the samples. */
  close(): void {
    this.#final = this.summary();
    this.#counts.clear();
    this.#sorted = new Float64Array(0);
  }

  summary(): LatencySummary | null {
    if (this.#final !== undefined) return this.#final;
    if (this.#cached !== undefined) return this.#cached;
    if (!this.count && !this.#unresolved && !this.#sendFailures && this.#complete) return (this.#cached = null);
    const keys = this.#merge();
    const cumulative = new Float64Array(keys.length);
    let total = 0;
    for (let i = 0; i < keys.length; i++) cumulative[i] = total += this.#counts.get(keys[i]!)!;
    const n = this.#rtts;
    // The value at a 0-based rank among all samples, found through the cumulative counts.
    const at = (rank: number) => {
      let [lo, hi] = [0, keys.length - 1];
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (cumulative[mid]! > rank) hi = mid;
        else lo = mid + 1;
      }
      return keys[lo]! / 1e6;
    };
    const nearest = (p: number) => (n ? at(nearestRank(p, n)) : null);
    const { count, raw, handling } = this.#timing;
    return (this.#cached = {
      ...(count && {
        reflectorTiming: { sampleCount: count, meanRawRttMs: raw / count, meanHandlingMs: handling / count },
      }),
      accountingComplete: this.#complete,
      probeCount: this.count,
      timeoutCount: this.#timeouts,
      unresolvedCount: this.#unresolved,
      sendFailureCount: this.#sendFailures,
      jitterPairs: this.#jitterPairs,
      minMs: n ? at(0) : null,
      maxMs: n ? at(n - 1) : null,
      meanMs: n ? this.#sum / n : null,
      p10Ms: nearest(0.1),
      p50Ms: n ? midpointMedian(n, at) : null,
      p90Ms: nearest(0.9),
      p95Ms: nearest(0.95),
      jitterMs: this.#jitterPairs ? this.#jitterSum / this.#jitterPairs : null,
    });
  }

  /** Merges the values first seen since the last summary into the sorted distinct values. */
  #merge(): Float64Array {
    const n = this.#sorted.length;
    const k = this.#fresh.length;
    if (!k) return this.#sorted;
    let buffer = new Float64Array(this.#sorted.buffer);
    if (buffer.length < n + k) {
      buffer = new Float64Array(2 * (n + k));
      buffer.set(this.#sorted);
    }
    const fresh = Float64Array.from(this.#fresh).sort();
    for (let i = n - 1, j = k - 1, w = n + k - 1; j >= 0; w--)
      buffer[w] = i >= 0 && buffer[i]! > fresh[j]! ? buffer[i--]! : fresh[j--]!;
    this.#fresh = [];
    return (this.#sorted = buffer.subarray(0, n + k));
  }
}

/** The idle stage's last four seconds of outcomes, which early finish scores. */
class IdleWindow {
  #times: number[] = [];
  /** RTTs, NaN for a timeout. */
  #rtts: number[] = [];
  #head = 0;
  #scratch = new Float64Array(1024);

  observe(atMs: number, outcome: Outcome): void {
    if (!outcome.inWindow) return;
    this.#times.push(atMs);
    this.#rtts.push(outcome.timedOut ? NaN : outcome.rttMs);
    while (this.#times[this.#head]! <= atMs - SCORE_WINDOW_MS) this.#head++;
    // Trimming in bulk keeps a dense reply-driven window linear.
    if (this.#head < 4096) return;
    this.#times.splice(0, this.#head);
    this.#rtts.splice(0, this.#head);
    this.#head = 0;
  }

  reset(): void {
    this.#times = [];
    this.#rtts = [];
    this.#head = 0;
  }

  /** 1 − 1.2 × (median deviation / max(median, 20 ms)) − 3.6 × timeout ratio. */
  confidence(): Confidence {
    const latest = this.#times.at(-1) ?? 0;
    if (this.#scratch.length < this.#times.length) this.#scratch = new Float64Array(this.#times.length * 2);
    const values = this.#scratch;
    let outcomes = 0;
    let replies = 0;
    for (let i = this.#head; i < this.#times.length; i++) {
      if (this.#times[i]! <= latest - SCORE_WINDOW_MS) continue;
      outcomes++;
      if (!Number.isNaN(this.#rtts[i])) values[replies++] = this.#rtts[i]!;
    }
    if (replies < 2) return { score: 0, sampleCount: outcomes };
    const center = selectMedian(values, replies);
    for (let i = 0; i < replies; i++) values[i] = Math.abs(values[i]! - center);
    const deviation = selectMedian(values, replies) / Math.max(center, 20);
    const timeoutRatio = (outcomes - replies) / outcomes;
    return { score: Math.min(1, Math.max(0, 1 - deviation * 1.2 - timeoutRatio * 3.6)), sampleCount: outcomes };
  }
}

/** The k-th smallest of the first n values, by selection; smaller values end up before it. */
function select(values: Float64Array, n: number, k: number): number {
  let lo = 0;
  let hi = n - 1;
  while (lo < hi) {
    const pivot = values[(lo + hi) >> 1]!;
    let i = lo;
    let j = hi;
    while (i <= j) {
      while (values[i]! < pivot) i++;
      while (values[j]! > pivot) j--;
      if (i > j) break;
      [values[i], values[j]] = [values[j]!, values[i]!];
      i++;
      j--;
    }
    if (k <= j) hi = j;
    else if (k >= i) lo = i;
    else break;
  }
  return values[k]!;
}

function selectMedian(values: Float64Array, n: number): number {
  const upper = select(values, n, n >> 1);
  if (n % 2) return upper;
  let lower = -Infinity;
  for (let i = 0; i < n >> 1; i++) lower = Math.max(lower, values[i]!);
  return (lower + upper) / 2;
}

/** RFC 6298 smoothing (α = 1/8, β = 1/4) for the probe deadline. */
export class RttEstimator {
  #srtt = 0;
  #rttvar = 0;
  #seen = false;

  observe(rttMs: number): void {
    if (!this.#seen) [this.#srtt, this.#rttvar, this.#seen] = [rttMs, rttMs / 2, true];
    else {
      this.#rttvar = 0.75 * this.#rttvar + 0.25 * Math.abs(this.#srtt - rttMs);
      this.#srtt = 0.875 * this.#srtt + 0.125 * rttMs;
    }
  }

  /** SRTT + 4 × max(RTTVAR, 1 ms), within `floor` and `ceiling`; 250 ms before the first reply. */
  deadlineMs(floor = DEADLINE_FLOOR_MS, ceiling = DEADLINE_CEILING_MS): number {
    if (!this.#seen) return DEADLINE_FLOOR_MS;
    return Math.min(Math.max(this.#srtt + 4 * Math.max(1, this.#rttvar), floor), ceiling);
  }
}

export const DEADLINE_FLOOR_MS = 250;
export const DEADLINE_CEILING_MS = 10_000;

/** The idle headline: the latency stage's median, its jitter and their ratio as stability. */
export interface LatencyResult {
  reportedMs: number;
  jitterMs: number | null;
  stabilityPct: number | null;
}

/** Each loaded stage's median minus the idle median, in signed ms. */
export type AddedLatency = Record<TransferStage, number | null>;

/** A failed population names its median only from this many resolved probes. */
const MIN_FAILED_OUTCOMES = 3;

/** One server's populations, one per stage: idle and loaded RTTs, and servers, are never pooled. */
export class ServerLatency {
  readonly stages: Record<Stage, Population> = {
    latency: new Population(),
    download: new Population(),
    upload: new Population(),
    bidirectional: new Population(),
  };
  readonly failed = new Set<Stage>();
  /** The idle stage's recent outcomes, for early finish. */
  readonly idle = new IdleWindow();

  observe(stage: Stage, outcome: Outcome, t: number, continuity: number): void {
    this.stages[stage].observe(outcome, continuity);
    if (stage === "latency") this.idle.observe(t, outcome);
  }

  fail(stage: Stage): void {
    this.failed.add(stage);
    this.stages[stage].markIncomplete();
  }

  close(): void {
    for (const population of Object.values(this.stages)) population.close();
    this.idle.reset();
  }

  summary(stage: Stage): LatencySummary | null {
    const population = this.stages[stage];
    const summary = population.summary();
    return summary && this.failed.has(stage) && population.count < MIN_FAILED_OUTCOMES
      ? { ...summary, p50Ms: null }
      : summary;
  }

  summaries(): Record<Stage, LatencySummary | null> {
    return {
      latency: this.summary("latency"),
      download: this.summary("download"),
      upload: this.summary("upload"),
      bidirectional: this.summary("bidirectional"),
    };
  }

  result(): LatencyResult | null {
    const summary = this.summary("latency");
    if (summary?.p50Ms == null) return null;
    const { p50Ms, jitterMs } = summary;
    return {
      reportedMs: p50Ms,
      jitterMs,
      stabilityPct: jitterMs === null ? null : Math.max(0, 100 * (1 - jitterMs / Math.max(p50Ms, 1))),
    };
  }

  /** Null without an idle median or any loaded one; a negative difference stays negative. */
  addedLatency(): AddedLatency | null {
    const idle = this.summary("latency")?.p50Ms;
    const loaded = (stage: TransferStage) => this.summary(stage)?.p50Ms ?? null;
    const added = { download: loaded("download"), upload: loaded("upload"), bidirectional: loaded("bidirectional") };
    if (idle == null || Object.values(added).every((p50) => p50 === null)) return null;
    for (const stage of ["download", "upload", "bidirectional"] as const)
      if (added[stage] !== null) added[stage] -= idle;
    return added;
  }
}
