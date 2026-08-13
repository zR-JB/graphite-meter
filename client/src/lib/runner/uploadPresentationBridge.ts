/** Presentation-only policy for an irregular authoritative upload feed. It has
 * no access to accumulators, charts, control buckets, or result reduction. */
export const UPLOAD_PRESENTATION_HINT_MAX_AGE_MS = 250;

export class UploadPresentationBridge {
  #arrivals: number[] = [];
  #lastRate = 0;
  #hint: { rate: number; at: number } | null = null;

  authoritative(rate: number, advancing: boolean, now: number): void {
    if (!advancing || rate <= 0) return;
    this.#arrivals.push(now);
    if (this.#arrivals.length > 5) this.#arrivals.shift();
    this.#lastRate = rate;
    this.#hint = null;
  }

  hint(bytes: number, elapsedMs: number, now: number): void {
    if (bytes <= 0 || elapsedMs <= 0) return;
    this.#hint = { rate: bytes / (elapsedMs / 1_000), at: now };
  }

  target(now: number, healthy: boolean): number | null {
    const hint = this.#hint;
    if (!healthy || !hint || this.#lastRate <= 0 || this.#arrivals.length < 4)
      return null;
    if (now - hint.at > UPLOAD_PRESENTATION_HINT_MAX_AGE_MS) return null;
    const gaps = this.#arrivals.slice(1).map((t, i) => t - this.#arrivals[i]);
    const ordered = [...gaps].sort((a, b) => a - b);
    const median = ordered[Math.floor(ordered.length / 2)] ?? 0;
    const last = this.#arrivals.at(-1) ?? now;
    if (now - last <= Math.max(300, 3 * median)) return null;
    return Math.max(
      this.#lastRate * 0.75,
      Math.min(this.#lastRate * 1.25, hint.rate),
    );
  }

  stop(): void {
    this.#hint = null;
  }
}
