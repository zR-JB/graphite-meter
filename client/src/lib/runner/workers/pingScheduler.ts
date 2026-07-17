export interface PingSchedulerClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(timer: unknown): void;
}

export type PingPacing =
  | { kind: "fixed"; intervalMs: number }
  | { kind: "reply-driven"; backupDelayMs: () => number };

const systemClock: PingSchedulerClock = {
  now: () => performance.now(),
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer as ReturnType<typeof setTimeout>),
};

/** Drives either a fixed start-to-start cadence or a reply-driven chain with a
 * bounded backup pacer. */
export class PingScheduler {
  #lastSendAt: number | null = null;
  #timer: unknown = null;
  #running = false;

  constructor(
    private pacing: PingPacing,
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
    this.pacing = { kind: "fixed", intervalMs };
    if (this.#running) this.#trySend();
  }

  /** A PING completed. Reply-driven mode continues immediately; fixed mode
   * sends only when its start-to-start boundary is already due. */
  complete(): void {
    if (!this.#running) return;
    if (this.pacing.kind === "reply-driven") {
      this.#sendAndArm();
      return;
    }
    this.#trySend();
  }

  #trySend(): void {
    if (!this.#running) return;
    if (this.pacing.kind === "reply-driven") {
      this.#sendAndArm();
      return;
    }
    const now = this.clock.now();
    const dueAt =
      this.#lastSendAt === null
        ? now
        : this.#lastSendAt + this.pacing.intervalMs;
    if (now < dueAt) {
      this.#arm(dueAt - now);
      return;
    }
    if (this.send(now)) {
      this.#lastSendAt = now;
      this.#arm(this.pacing.intervalMs);
    }
  }

  #sendAndArm(): void {
    if (this.#timer !== null) this.clock.clearTimeout(this.#timer);
    this.#timer = null;
    const now = this.clock.now();
    if (!this.send(now)) return;
    this.#lastSendAt = now;
    this.#arm(
      this.pacing.kind === "reply-driven"
        ? this.pacing.backupDelayMs()
        : this.pacing.intervalMs,
    );
  }

  #arm(delayMs: number): void {
    if (this.#timer !== null) this.clock.clearTimeout(this.#timer);
    this.#timer = this.clock.setTimeout(() => {
      this.#timer = null;
      this.#trySend();
    }, delayMs);
  }
}
