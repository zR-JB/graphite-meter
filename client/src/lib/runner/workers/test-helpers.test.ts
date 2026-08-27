const globals = globalThis as Record<string, unknown>;

export const messageEvent = <T>(data: T): MessageEvent<T> =>
  ({ data }) as MessageEvent<T>;

export function stubFetch(handler: typeof fetch): () => void {
  const real = globalThis.fetch;
  globalThis.fetch = handler;
  return () => {
    globalThis.fetch = real;
  };
}

export function testClock() {
  let time = 0;
  let nextId = 0;
  const timers = new Map<number, { at: number; callback: () => void }>();
  return {
    now: () => time,
    setTimeout(callback: () => void, delayMs: number) {
      const id = nextId++;
      timers.set(id, { at: time + delayMs, callback });
      return id;
    },
    clearTimeout(timer: unknown) {
      timers.delete(timer as number);
    },
    advance(ms: number) {
      const end = time + ms;
      for (;;) {
        const next = [...timers.entries()]
          .filter(([, timer]) => timer.at <= end)
          .sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0];
        if (!next) break;
        time = next[1].at;
        timers.delete(next[0]);
        next[1].callback();
      }
      time = end;
    },
  };
}

export interface WorkerRealm<Out> {
  posted: Out[];
  send(message: unknown): void;
}

export async function bootWorker<Out>(
  modulePath: string,
  realm: number,
): Promise<WorkerRealm<Out>> {
  const posted: Out[] = [];
  globals.postMessage = (message: Out): void => {
    posted.push(message);
  };
  await import(`${modulePath}?realm=${realm}`);
  const handler = globalThis.onmessage as (event: MessageEvent) => void;
  return {
    posted,
    send: (message) => handler(messageEvent(message)),
  };
}
