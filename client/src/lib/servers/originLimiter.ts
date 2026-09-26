/** Bounds connection checks: 8 in total, 2 per origin; background work never fills the pool. */
export function originLimiter() {
  type Item = { origin: string; priority: () => number; start: () => void };
  let active = 0;
  let background = 0;
  const origins = new Map<string, number>();
  let queue: Item[] = [];

  function drain(): void {
    queue.sort((a, b) => a.priority() - b.priority());
    while (active < 8 && queue.length) {
      const index = queue.findIndex((item) => {
        const busy = origins.get(item.origin) ?? 0;
        return item.priority() === 0 ? busy < 2 : busy === 0 && background < 4;
      });
      if (index < 0) break;
      queue.splice(index, 1)[0].start();
    }
  }

  function acquire(
    origin: string,
    priority: () => number,
    signal: AbortSignal,
  ): Promise<() => void> {
    return new Promise((resolve, reject) => {
      if (signal.aborted) return reject(signal.reason);
      const abort = () => {
        queue = queue.filter((queued) => queued !== item);
        reject(signal.reason);
      };
      const item: Item = {
        origin,
        priority,
        start: () => {
          signal.removeEventListener("abort", abort);
          const isBackground = priority() > 0;
          active++;
          if (isBackground) background++;
          origins.set(origin, (origins.get(origin) ?? 0) + 1);
          resolve(() => {
            active--;
            if (isBackground) background--;
            const remaining = origins.get(origin)! - 1;
            if (remaining) origins.set(origin, remaining);
            else origins.delete(origin);
            drain();
          });
        },
      };
      signal.addEventListener("abort", abort, { once: true });
      queue.push(item);
      drain();
    });
  }
  return { acquire, drain };
}
