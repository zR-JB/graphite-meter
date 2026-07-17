export interface PingSchedulerClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(timer: unknown): void;
}

const systemClock: PingSchedulerClock = {
  now: () => performance.now(),
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer as ReturnType<typeof setTimeout>),
};

/** Enforces a minimum start-to-start interval without catch-up bursts. */
export class PingScheduler {
  #lastSendAt: number | null = null;
  #timer: unknown = null;
  #running = false;

  constructor(
    private intervalMs: number,
    private readonly send: (now: number) => boolean,
    private readonly clock: PingSchedulerClock = systemClock,
  ) {}

  start(): void {
    this.#running = true;
    this.#trySend();
  }

  stop(): void {
    this.#running = false;
    if (this.#timer !== null) this.clock.clearTimeout(this.#timer);
    this.#timer = null;
  }

  reset(): void {
    this.stop();
    this.#lastSendAt = null;
  }

  /** Change cadence without resetting the last-send boundary or sending a
   * catch-up ping. */
  setInterval(intervalMs: number): void {
    this.intervalMs = intervalMs;
    if (this.#running) this.#trySend();
  }

  /** Capacity became available. Send now only when the cadence is already due. */
  nudge(): void {
    if (this.#running) this.#trySend();
  }

  #trySend(): void {
    if (!this.#running) return;
    const now = this.clock.now();
    const dueAt =
      this.#lastSendAt === null ? now : this.#lastSendAt + this.intervalMs;
    if (now < dueAt) {
      this.#arm(dueAt - now);
      return;
    }
    if (this.send(now)) {
      this.#lastSendAt = now;
      this.#arm(this.intervalMs);
    }
  }

  #arm(delayMs: number): void {
    if (this.#timer !== null) this.clock.clearTimeout(this.#timer);
    this.#timer = this.clock.setTimeout(() => {
      this.#timer = null;
      this.#trySend();
    }, delayMs);
  }
}
