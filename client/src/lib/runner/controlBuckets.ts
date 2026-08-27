/** Fixed-duration exact byte/time buckets for cadence-independent control. */

export const TRANSFER_CONTROL_BUCKET_MS = 250;

export class FixedRateBuckets {
  #bucketBytes = 0;
  #bucketDurationMs = 0;
  #completed: number[] = [];
  #evidenceMs = 0;
  #baseIndex = 0;

  constructor(private readonly maxCompleted = Number.POSITIVE_INFINITY) {}

  reset(): void {
    this.#bucketBytes = 0;
    this.#bucketDurationMs = 0;
    this.#completed = [];
    this.#evidenceMs = 0;
    this.#baseIndex = 0;
  }

  observe(bytesInput: number, durationInputMs: number): void {
    let bytes = Number.isFinite(bytesInput) ? Math.max(0, bytesInput) : 0;
    let durationMs = Number.isFinite(durationInputMs)
      ? Math.max(0, durationInputMs)
      : 0;
    if (durationMs <= 0) return;
    this.#evidenceMs += durationMs;
    while (durationMs > 0) {
      const remaining = TRANSFER_CONTROL_BUCKET_MS - this.#bucketDurationMs;
      const take = Math.min(remaining, durationMs);
      const fraction = take / durationMs;
      this.#bucketBytes += bytes * fraction;
      this.#bucketDurationMs += take;
      bytes -= bytes * fraction;
      durationMs -= take;
      if (
        this.#bucketDurationMs >=
        TRANSFER_CONTROL_BUCKET_MS - Number.EPSILON
      ) {
        const rate = (this.#bucketBytes * 1_000) / TRANSFER_CONTROL_BUCKET_MS;
        this.#completed.push(rate);
        if (this.#completed.length > this.maxCompleted) this.#completed.shift();
        this.#bucketBytes = 0;
        this.#bucketDurationMs = 0;
      }
    }
  }

  get rates(): readonly number[] {
    return this.#completed;
  }

  get completedCount(): number {
    return this.#completed.length;
  }

  get evidenceMs(): number {
    return this.#evidenceMs;
  }

  get baseIndex(): number {
    return this.#baseIndex;
  }

  dropFirst(count: number): void {
    const drop = Math.min(this.#completed.length, Math.max(0, count));
    if (!drop) return;
    this.#completed.splice(0, drop);
    this.#baseIndex += drop;
  }

  dropBefore(index: number): void {
    if (index <= this.#baseIndex) return;
    if (index > this.#baseIndex + this.#completed.length) {
      this.#bucketBytes = 0;
      this.#bucketDurationMs = 0;
    }
    const drop = Math.min(this.#completed.length, index - this.#baseIndex);
    if (drop) this.#completed.splice(0, drop);
    this.#baseIndex = index;
  }
}

/** Two lane windows retained and summed only at shared temporal bucket indexes. */
export class PairedRateBuckets {
  #down = new FixedRateBuckets();
  #up = new FixedRateBuckets();
  #temporal = false;
  #latestBucket = -1;
  #temporalDown = new Map<number, TemporalBucket>();
  #temporalUp = new Map<number, TemporalBucket>();

  constructor(private readonly maxCompleted = Number.POSITIVE_INFINITY) {}

  reset(): void {
    this.#down.reset();
    this.#up.reset();
    this.#temporal = false;
    this.#latestBucket = -1;
    this.#temporalDown.clear();
    this.#temporalUp.clear();
  }

  observe(
    direction: "down" | "up",
    bytes: number,
    durationMs: number,
    endAtMs?: number,
  ): void {
    if (Number.isFinite(endAtMs)) {
      this.#temporal = true;
      this.#observeTemporal(direction, bytes, durationMs, endAtMs as number);
      this.#trimTemporal();
      return;
    }
    // Keep cadence-only behavior for isolated callers; production supplies the shared run-clock timestamp above.
    if (this.#temporal) return;
    (direction === "down" ? this.#down : this.#up).observe(bytes, durationMs);
    this.#trim();
  }

  get rates(): readonly number[] {
    if (this.#temporal) return this.#temporalRates();
    const count = Math.min(this.#down.completedCount, this.#up.completedCount);
    return Array.from(
      { length: count },
      (_, index) => this.#down.rates[index] + this.#up.rates[index],
    );
  }

  get completedCount(): number {
    if (this.#temporal) return this.#temporalRates().length;
    return Math.min(this.#down.completedCount, this.#up.completedCount);
  }

  #observeTemporal(
    direction: "down" | "up",
    bytesInput: number,
    durationInputMs: number,
    endAtMs: number,
  ): void {
    const durationMs = Number.isFinite(durationInputMs)
      ? Math.max(0, durationInputMs)
      : 0;
    if (durationMs <= 0) return;
    const bytes = Number.isFinite(bytesInput) ? Math.max(0, bytesInput) : 0;
    const startAtMs = endAtMs - durationMs;
    if (!Number.isFinite(startAtMs)) return;
    const buckets =
      direction === "down" ? this.#temporalDown : this.#temporalUp;
    const endBucket = Math.ceil(endAtMs / TRANSFER_CONTROL_BUCKET_MS) - 1;
    this.#latestBucket = Math.max(this.#latestBucket, endBucket);
    this.#trimTemporal();
    const firstRetained =
      this.maxCompleted === Number.POSITIVE_INFINITY
        ? Number.NEGATIVE_INFINITY
        : this.#latestBucket - Math.max(0, this.maxCompleted) + 1;
    let atMs = Math.max(startAtMs, firstRetained * TRANSFER_CONTROL_BUCKET_MS);
    while (atMs < endAtMs) {
      const index = Math.floor(atMs / TRANSFER_CONTROL_BUCKET_MS);
      const bucketEndMs = (index + 1) * TRANSFER_CONTROL_BUCKET_MS;
      const takeMs = Math.min(endAtMs, bucketEndMs) - atMs;
      if (takeMs <= 0) break;
      if (index >= firstRetained) {
        const bucket = buckets.get(index) ?? { bytes: 0, durationMs: 0 };
        bucket.bytes += (bytes * takeMs) / durationMs;
        bucket.durationMs += takeMs;
        buckets.set(index, bucket);
      }
      atMs += takeMs;
    }
  }

  #temporalRates(): number[] {
    const indexes: number[] = [];
    for (const [index, down] of this.#temporalDown) {
      const up = this.#temporalUp.get(index);
      if (up && completeTemporalBucket(down) && completeTemporalBucket(up))
        indexes.push(index);
    }
    indexes.sort((a, b) => a - b);
    return indexes.map(
      (index) =>
        ((this.#temporalDown.get(index)!.bytes +
          this.#temporalUp.get(index)!.bytes) *
          1_000) /
        TRANSFER_CONTROL_BUCKET_MS,
    );
  }

  #trimTemporal(): void {
    if (this.maxCompleted === Number.POSITIVE_INFINITY) return;
    const first = this.#latestBucket - Math.max(0, this.maxCompleted) + 1;
    for (const buckets of [this.#temporalDown, this.#temporalUp])
      for (const index of buckets.keys())
        if (index < first) buckets.delete(index);
  }

  #trim(): void {
    const sharedBase = Math.max(this.#down.baseIndex, this.#up.baseIndex);
    this.#down.dropBefore(sharedBase);
    this.#up.dropBefore(sharedBase);
    this.#down.dropFirst(this.#down.completedCount - this.maxCompleted);
    this.#up.dropFirst(this.#up.completedCount - this.maxCompleted);
    const alignedBase = Math.max(this.#down.baseIndex, this.#up.baseIndex);
    this.#down.dropBefore(alignedBase);
    this.#up.dropBefore(alignedBase);
  }
}

interface TemporalBucket {
  bytes: number;
  durationMs: number;
}

const completeTemporalBucket = (bucket: TemporalBucket): boolean =>
  bucket.durationMs >= TRANSFER_CONTROL_BUCKET_MS - 1e-6;
