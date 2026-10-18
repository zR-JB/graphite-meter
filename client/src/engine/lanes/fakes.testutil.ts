// Gives the lanes' owners fake workers, which the code under test creates and the test speaks for, on a fake clock.
import { afterEach, beforeEach, jest } from "bun:test";
import type { LaneReport, ProbeReport, SessionReport } from "../../protocol/messages";

type Report = LaneReport | SessionReport | ProbeReport;

/** A worker that keeps its commands and reports what the test makes it report. */
export class FakeWorker {
  static spawned: FakeWorker[] = [];
  onmessage: ((event: { data: Report }) => void) | null = null;
  onerror: ((event: { message: string }) => void) | null = null;
  readonly commands: { type: string; [field: string]: unknown }[] = [];
  terminated = false;

  constructor(readonly url: string) {
    FakeWorker.spawned.push(this);
  }

  postMessage(command: { type: string }): void {
    this.commands.push(command);
  }

  terminate(): void {
    this.terminated = true;
  }

  report(data: Report): void {
    this.onmessage?.({ data });
  }
}

/** Every test of the calling file runs with fake workers and fake timers. */
export function useFakeWorkers(): void {
  const globals = { Worker: globalThis.Worker, WORKERS: Reflect.get(globalThis, "WORKERS") };
  beforeEach(() => {
    jest.useFakeTimers();
    FakeWorker.spawned = [];
    Object.assign(globalThis, {
      Worker: FakeWorker,
      WORKERS: { lane: "lane", session: "session", probe: "probe" },
    });
  });
  afterEach(() => {
    jest.useRealTimers();
    Object.assign(globalThis, globals);
  });
}

/** Moves the fake clock in 5 ms steps, letting promise chains settle before each step and after the last. */
export async function elapse(ms: number): Promise<void> {
  for (let at = 0; ; at += 5) {
    for (let turn = 0; turn < 20; turn++) await null;
    if (at >= ms) return;
    jest.advanceTimersByTime(Math.min(5, ms - at));
  }
}

/** Records when, on the fake clock, and how `promise` settled. */
export function outcome(promise: Promise<unknown>): { at?: number; error?: unknown } {
  const start = performance.now();
  const ended: { at?: number; error?: unknown } = {};
  promise.then(
    () => (ended.at = performance.now() - start),
    (error) => Object.assign(ended, { at: performance.now() - start, error }),
  );
  return ended;
}
