export interface PingSchedulerClock {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(timer: unknown): void;
}

type PingPacing =
  | { kind: "fixed"; intervalMs: number }
  | { kind: "reply-driven"; backupDelayMs: () => number };

const systemClock: PingSchedulerClock = {
  now: () => performance.now(),
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (timer) => clearTimeout(timer as ReturnType<typeof setTimeout>),
};

/** Drives either a fixed start-to-start cadence or a reply-driven chain with a bounded backup pacer. */
export interface PingScheduler {
  start(): void;
  stop(): void;
  reset(): void;
  restartNow(): void;
  setInterval(intervalMs: number): void;
  complete(): void;
}

export function createPingScheduler(
  initialPacing: PingPacing,
  send: (now: number) => boolean,
  clock: PingSchedulerClock = systemClock,
): PingScheduler {
  let pacing = initialPacing;
  let lastSendAt: number | null = null;
  let timer: unknown = null;
  let running = false;

  const arm = (delayMs: number): void => {
    if (timer !== null) clock.clearTimeout(timer);
    timer = clock.setTimeout(() => {
      timer = null;
      trySend();
    }, delayMs);
  };
  const sendAndArm = (backupDelayMs: () => number): void => {
    if (timer !== null) clock.clearTimeout(timer);
    timer = null;
    const now = clock.now();
    if (!send(now)) return;
    lastSendAt = now;
    arm(backupDelayMs());
  };
  function trySend(): void {
    if (!running) return;
    if (pacing.kind === "reply-driven") {
      sendAndArm(pacing.backupDelayMs);
      return;
    }
    const now = clock.now();
    const dueAt = lastSendAt === null ? now : lastSendAt + pacing.intervalMs;
    if (now < dueAt) {
      arm(dueAt - now);
      return;
    }
    if (send(now)) {
      lastSendAt = now;
      arm(pacing.intervalMs);
    }
  }

  return {
    start(): void {
      running = true;
      trySend();
    },
    stop(): void {
      running = false;
      if (timer !== null) clock.clearTimeout(timer);
      timer = null;
    },
    reset(): void {
      this.stop();
      lastSendAt = null;
    },
    restartNow(): void {
      if (!running) return;
      if (timer !== null) clock.clearTimeout(timer);
      timer = null;
      lastSendAt = null;
      trySend();
    },
    setInterval(intervalMs: number): void {
      pacing = { kind: "fixed", intervalMs };
      if (running) trySend();
    },
    complete(): void {
      if (!running) return;
      if (pacing.kind === "reply-driven") sendAndArm(pacing.backupDelayMs);
      else trySend();
    },
  };
}
