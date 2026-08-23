/** Fixed-duration exact byte/time buckets for cadence-independent control. */

export const TRANSFER_CONTROL_BUCKET_MS = 250;

export class FixedRateBuckets {
  #bucketBytes = 0;
  #bucketDurationMs = 0;
  #completed: number[] = [];
  #evidenceMs = 0;

  constructor(private readonly maxCompleted = Number.POSITIVE_INFINITY) {}

  reset(): void {
    this.#bucketBytes = 0;
    this.#bucketDurationMs = 0;
    this.#completed = [];
    this.#evidenceMs = 0;
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
}
