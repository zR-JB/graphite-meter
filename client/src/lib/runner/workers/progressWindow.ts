import { REPORT_GAP_MS } from "./tuning";

export interface ProgressDelta {
  bytes: number;
  elapsedMs: number;
}

/** Owns the byte/time window shared by fetch and WebTransport downloads. */
export class ProgressWindow {
  #bytes = 0;
  #startedAt: number;
  /** Deltas are batched to this cadence before crossing the thread. */
  #gapMs: number;

  constructor(now = performance.now(), gapMs = REPORT_GAP_MS) {
    this.#startedAt = now;
    this.#gapMs = gapMs;
  }

  reset(now = performance.now()): void {
    this.#bytes = 0;
    this.#startedAt = now;
  }

  add(bytes: number, now = performance.now()): ProgressDelta | null {
    this.#bytes += bytes;
    if (now - this.#startedAt < this.#gapMs) return null;
    return this.flush(now);
  }

  flush(now = performance.now()): ProgressDelta | null {
    const delta = { bytes: this.#bytes, elapsedMs: now - this.#startedAt };
    this.reset(now);
    return delta.bytes > 0 && delta.elapsedMs > 0 ? delta : null;
  }
}
