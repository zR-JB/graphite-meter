import type {
  LatencyResult,
  ReflectorTimingSummary,
  StageLatencySummary,
  TransportRole,
} from "./contract";
import { median } from "./stats";

/** One presentation of a stage population, shared by live results and history. */
export interface LatencyLaneSnapshot {
  reflectorTiming?: ReflectorTimingSummary;
  min: number | null;
  max: number | null;
  p10: number | null;
  p90: number | null;
  p95?: number | null;
  /** Median (p50) of every population; the idle headline may use its stable window. */
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
  result: LatencyResult | null,
  summaries: Partial<Record<TransportRole, StageLatencySummary | null>>,
): Record<TransportRole, LatencyLaneSnapshot | null> {
  const lane = (stage: TransportRole): LatencyLaneSnapshot | null => {
    const summary = summaries[stage];
    return summary
      ? {
          ...(summary.reflectorTiming
            ? { reflectorTiming: { ...summary.reflectorTiming } }
            : {}),
          min: summary.minMs,
          max: summary.maxMs,
          p10: summary.p10Ms,
          p90: summary.p90Ms,
          p95: summary.p95Ms,
          center:
            (stage === "latency" ? result?.reportedMs : null) ?? summary.p50Ms,
          jitter: summary.jitterMs,
          timeoutRatio: summary.probeCount
            ? summary.timeoutCount / summary.probeCount
            : null,
          accountingComplete: summary.accountingComplete,
          timeoutCount: summary.timeoutCount,
          unresolvedCount: summary.unresolvedCount,
          sendFailureCount: summary.sendFailureCount,
          count: summary.probeCount,
        }
      : null;
  };
  return {
    latency: lane("latency"),
    download: lane("download"),
    upload: lane("upload"),
    bidirectional: lane("bidirectional"),
  };
}

/** Raw outcomes own statistics; chart buckets never feed this accumulator. */
export class LatencyAccumulator {
  readonly rtts: number[] = [];
  #timeouts = 0;
  #replies = 0;
  #unresolved = 0;
  #sendFailures = 0;
  #sum = 0;
  #timingCount = 0;
  #timingRawSum = 0;
  #handlingSum = 0;
  #deltaSum = 0;
  #deltaCount = 0;
  #previous: number | null = null;
  #continuityId = 0;
  #accountingComplete = true;
  #snapshot: StageLatencySummary | null | undefined;
  #windowMedian: { start: number; count: number; value: number } | undefined;

  get count(): number {
    return this.#replies + this.#timeouts;
  }

  get probeTimeoutPct(): number | null {
    return this.count ? (100 * this.#timeouts) / this.count : null;
  }

  observe(
    rttMs: number,
    timedOut: boolean,
    continuityId: number,
    rttEligible = true,
    reflectorHandlingMs?: number,
  ): void {
    this.#snapshot = undefined;
    if (continuityId !== this.#continuityId) this.#previous = null;
    this.#continuityId = continuityId;
    if (timedOut) {
      this.#timeouts++;
      return;
    }
    if (!Number.isFinite(rttMs) || rttMs < 0) return;
    this.#replies++;
    if (!rttEligible) return;
    this.rtts.push(rttMs);
    this.#sum += rttMs;
    if (
      reflectorHandlingMs !== undefined &&
      Number.isFinite(reflectorHandlingMs) &&
      reflectorHandlingMs >= 0 &&
      reflectorHandlingMs <= rttMs
    ) {
      this.#timingCount++;
      this.#timingRawSum += rttMs;
      this.#handlingSum += reflectorHandlingMs;
    }
    if (this.#previous !== null) {
      this.#deltaSum += Math.abs(rttMs - this.#previous);
      this.#deltaCount++;
    }
    this.#previous = rttMs;
  }

  interrupt(count: number, reason: "unresolved" | "send-failed"): void {
    if (!Number.isSafeInteger(count) || count <= 0) return;
    this.#snapshot = undefined;
    if (reason === "unresolved") this.#unresolved += count;
    else this.#sendFailures += count;
    this.#previous = null;
  }

  markAccountingIncomplete(): void {
    this.#accountingComplete = false;
    this.#snapshot = undefined;
    this.#previous = null;
  }

  /** Exact selected-window median; unchanged completed populations need no re-sorting. */
  medianFrom(start: number): number {
    if (start <= 0) return this.snapshot()?.p50Ms ?? 0;
    const cached = this.#windowMedian;
    if (cached?.start === start && cached.count === this.rtts.length)
      return cached.value;
    const value = median(this.rtts.slice(start));
    this.#windowMedian = { start, count: this.rtts.length, value };
    return value;
  }

  snapshot(): StageLatencySummary | null {
    if (this.#snapshot !== undefined) return this.#snapshot;
    if (
      !this.count &&
      !this.#unresolved &&
      !this.#sendFailures &&
      this.#accountingComplete
    )
      return (this.#snapshot = null);
    const sorted = [...this.rtts].sort((a, b) => a - b);
    const rank = (p: number): number | null =>
      sorted.length
        ? sorted[Math.max(0, Math.ceil(p * sorted.length) - 1)]
        : null;
    const mid = Math.floor(sorted.length / 2);
    return (this.#snapshot = {
      ...(this.#timingCount
        ? {
            reflectorTiming: {
              sampleCount: this.#timingCount,
              meanRawRttMs: this.#timingRawSum / this.#timingCount,
              meanHandlingMs: this.#handlingSum / this.#timingCount,
              meanAdjustedRttMs:
                (this.#timingRawSum - this.#handlingSum) / this.#timingCount,
            },
          }
        : {}),
      accountingComplete: this.#accountingComplete,
      probeCount: this.count,
      timeoutCount: this.#timeouts,
      unresolvedCount: this.#unresolved,
      sendFailureCount: this.#sendFailures,
      jitterPairs: this.#deltaCount,
      minMs: sorted[0] ?? null,
      maxMs: sorted.at(-1) ?? null,
      meanMs: sorted.length ? this.#sum / sorted.length : null,
      p10Ms: rank(0.1),
      p50Ms: sorted.length
        ? sorted.length % 2
          ? sorted[mid]
          : (sorted[mid - 1] + sorted[mid]) / 2
        : null,
      p90Ms: rank(0.9),
      p95Ms: rank(0.95),
      jitterMs: this.#deltaCount ? this.#deltaSum / this.#deltaCount : null,
    });
  }
}
