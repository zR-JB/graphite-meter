const TIMER_GAP_MS = 1500;

/** Epoch ms for stamps compared with server expiry; page ms for timing outside a run. */
export const epochMs = (): number => Date.now();
export const pageMs = (): number => performance.now();

/** One run's time: the first reading after a page timer gap counts it, and active time leaves gaps out. */
export class RunClock {
  readonly start = pageMs();
  readonly startedAt = epochMs();
  #read = this.start;
  #paused = 0;
  #gapped = false;
  #held = false;

  read(): number {
    const at = pageMs();
    if (!this.#held && at - this.#read > TIMER_GAP_MS) {
      this.#paused += at - this.#read;
      this.#gapped = true;
    }
    this.#read = at;
    return at;
  }

  active(): number {
    return this.read() - this.start - this.#paused;
  }

  takeGap(): boolean {
    const gapped = this.#gapped;
    this.#gapped = false;
    return gapped;
  }

  get held(): boolean {
    return this.#held;
  }

  /** Stage preparation and finalization hold the timeline; that time is never a gap. */
  hold(): void {
    this.#held = true;
  }

  resume(): number {
    this.#held = false;
    return (this.#read = pageMs());
  }
}
