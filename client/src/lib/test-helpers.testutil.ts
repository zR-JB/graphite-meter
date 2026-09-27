import { jest } from "bun:test";

/** Replace test globals and restore their original descriptors, including absent properties. */
export function stubGlobals(values: Record<string, unknown>): () => void {
  const previous = Object.entries(values).map(([key, value]) => {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, key);
    Object.defineProperty(globalThis, key, {
      value,
      configurable: true,
      writable: true,
    });
    return [key, descriptor] as const;
  });
  return () => {
    for (const [key, descriptor] of previous.toReversed()) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else Reflect.deleteProperty(globalThis, key);
    }
  };
}

/** The trimmed `|` cells of each record in a pin the Go side asserts too. */
export async function readPin(name: string): Promise<string[][]> {
  const text = await Bun.file(
    new URL(`../../../api/${name}`, import.meta.url),
  ).text();
  const rows = text
    .split("\n")
    .filter((line) => line.trim() && !line.startsWith("#"))
    .map((line) => line.split("|").map((cell) => cell.trim()));
  if (rows.length === 0) throw new Error(`api/${name} is empty`);
  return rows;
}

/** One real task turn: every queued continuation settles, with or without fake timers. */
export const taskTurn = (): Promise<void> =>
  new Promise((resolve) => {
    const { port1, port2 } = new MessageChannel();
    port1.onmessage = (): void => {
      port1.close();
      port2.close();
      resolve();
    };
    port2.postMessage(0);
  });

/** Task turns; a fake clock moves 1 ms per turn, as it would behind a real 0 ms timer. */
export async function until(done: () => boolean, turns = 100): Promise<void> {
  for (let turn = 0; !done(); turn++) {
    if (turn === turns)
      throw new Error(`condition not met within ${turns} turns`);
    await taskTurn();
    if (!done() && jest.isFakeTimers()) jest.advanceTimersByTime(1);
  }
}

/** Enough task turns for stubbed I/O to finish before asserting that something did not happen. */
export async function settle(): Promise<void> {
  for (let i = 0; i < 10; i++) await taskTurn();
}

export async function elapse(ms: number): Promise<void> {
  for (let elapsed = 0; elapsed < ms; elapsed += 5) {
    await taskTurn();
    jest.advanceTimersByTime(Math.min(5, ms - elapsed));
  }
  await taskTurn();
}
