// Bounds concurrent connection checks overall and per origin; background work runs only on an idle origin.
const TOTAL = 8;
const PER_ORIGIN = 2;
const BACKGROUND = 4;

export interface Limiter {
  /** Resolves with the slot's release once a check may start; `priority` 0 is a selected server, read again on every drain. */
  acquire(origin: string, priority: () => number, signal: AbortSignal): Promise<() => void>;
  /** Starts what now fits, after priorities changed. */
  drain(): void;
}

export function createLimiter(): Limiter {
  type Waiting = { origin: string; priority: () => number; start(): void };
  const running = new Map<string, number>();
  let total = 0;
  let background = 0;
  let queue: Waiting[] = [];

  const fits = ({ origin, priority }: Waiting) => {
    const busy = running.get(origin) ?? 0;
    return priority() === 0 ? busy < PER_ORIGIN : busy === 0 && background < BACKGROUND;
  };
  function drain(): void {
    queue.sort((a, b) => a.priority() - b.priority());
    while (total < TOTAL) {
      const index = queue.findIndex(fits);
      if (index < 0) return;
      queue.splice(index, 1)[0]!.start();
    }
  }
  function acquire(
    origin: string,
    priority: () => number,
    signal: AbortSignal,
  ): Promise<() => void> {
    const { promise, resolve, reject } = Promise.withResolvers<() => void>();
    if (signal.aborted) {
      reject(signal.reason);
      return promise;
    }
    const abort = () => {
      queue = queue.filter((each) => each !== waiting);
      reject(signal.reason);
    };
    const waiting: Waiting = {
      origin,
      priority,
      start() {
        signal.removeEventListener("abort", abort);
        // Whether a check counts as background is fixed when it starts.
        const isBackground = priority() > 0;
        total++;
        if (isBackground) background++;
        running.set(origin, (running.get(origin) ?? 0) + 1);
        let released = false;
        resolve(() => {
          if (released) return;
          released = true;
          total--;
          if (isBackground) background--;
          const left = running.get(origin)! - 1;
          if (left) running.set(origin, left);
          else running.delete(origin);
          drain();
        });
      },
    };
    signal.addEventListener("abort", abort, { once: true });
    queue.push(waiting);
    drain();
    return promise;
  }
  return { acquire, drain };
}
