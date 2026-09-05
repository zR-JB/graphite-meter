import type { StageLatencySummary } from "./contract";

/** Raw outcomes own statistics; chart buckets never feed this accumulator. */
export class LatencyAccumulator {
  readonly rtts: number[] = [];
  #timeouts = 0;
  #replies = 0;
  #unresolved = 0;
  #sendFailures = 0;
  #sum = 0;
  #deltaSum = 0;
  #deltaCount = 0;
  #previous: number | null = null;
  #continuityId = 0;
  #accountingComplete = true;

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
  ): void {
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

  markAccountingIncomplete(): void {
    this.#accountingComplete = false;
    this.#previous = null;
  }

  snapshot(): StageLatencySummary | null {
    if (
      !this.count &&
      !this.#unresolved &&
      !this.#sendFailures &&
      this.#accountingComplete
    )
      return null;
    const sorted = [...this.rtts].sort((a, b) => a - b);
    const rank = (p: number): number | null =>
      sorted.length
        ? sorted[Math.max(0, Math.ceil(p * sorted.length) - 1)]
        : null;
    const mid = Math.floor(sorted.length / 2);
    return {
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
    };
  }
}
