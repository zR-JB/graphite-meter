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

/** Two lane windows trimmed by one shared bucket index. */
export class PairedRateBuckets {
  #down = new FixedRateBuckets();
  #up = new FixedRateBuckets();

  constructor(private readonly maxCompleted = Number.POSITIVE_INFINITY) {}

  reset(): void {
    this.#down.reset();
    this.#up.reset();
  }

  observe(direction: "down" | "up", bytes: number, durationMs: number): void {
    (direction === "down" ? this.#down : this.#up).observe(bytes, durationMs);
    this.#trim();
  }

  get rates(): readonly number[] {
    const count = Math.min(this.#down.completedCount, this.#up.completedCount);
    return Array.from(
      { length: count },
      (_, index) => this.#down.rates[index] + this.#up.rates[index],
    );
  }

  get completedCount(): number {
    return Math.min(this.#down.completedCount, this.#up.completedCount);
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
