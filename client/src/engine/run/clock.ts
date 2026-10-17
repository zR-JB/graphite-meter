// The main thread's wall clock and timers, and one run's time, so tests can drive both.
import { pageMs } from "../../lib/frame";
import { workerScript } from "../../lib/trusted";

/** Wall-clock milliseconds since the epoch, for timestamps that leave the page. */
export const epochMs = (): number => Date.now();

/** The page time of an epoch time; worker clocks cross realms only as epoch values. */
export const fromEpoch = (epoch: number): number => epoch - performance.timeOrigin;

/** The time a run reads; tests pass a fake. */
export interface Clock {
  now(): number;
  epoch(): number;
  /** Runs `fn` once after `ms`; the returned function cancels it. */
  after(ms: number, fn: () => void): () => void;
}

let worker: Worker | null | undefined;
let nextId = 0;
const due = new Map<number, () => void>();

// A tab hidden for a few minutes wakes its own timers about once a minute, which would split a long run's evidence
// at every wake; a worker's timers keep their pace.
function timerWorker(): Worker | null {
  if (worker !== undefined) return worker;
  worker = null;
  if (typeof document === "undefined") return worker;
  try {
    worker = new Worker(workerScript(WORKERS.clock), { type: "module" });
    worker.onmessage = ({ data: id }: MessageEvent<number>) => due.get(id)?.();
  } catch {}
  return worker;
}

function after(ms: number, fn: () => void): () => void {
  const host = timerWorker();
  const id = ++nextId;
  // The page timer backs the worker up: one that failed to load or hung never answers.
  const backstop = setTimeout(() => {
    cancel();
    fn();
  }, ms);
  const cancel = () => {
    clearTimeout(backstop);
    if (due.delete(id)) host!.postMessage({ id, ms: null });
  };
  if (host) {
    due.set(id, () => {
      clearTimeout(backstop);
      due.delete(id);
      fn();
    });
    host.postMessage({ id, ms });
  }
  return cancel;
}

export const pageClock: Clock = { now: pageMs, epoch: epochMs, after };

/** Resolves after `ms` on the page's timers, or at once when `signal` aborts; the timer never outlives either. */
export function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const done = () => {
      cancel();
      signal.removeEventListener("abort", done);
      resolve();
    };
    const cancel = after(ms, done);
    signal.addEventListener("abort", done);
  });
}

/** `promise`, or its signal's reason as soon as that aborts; the operation still owns its cleanup. */
export function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason);
    if (signal.aborted) abort();
    else signal.addEventListener("abort", abort);
    promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

const GAP_MS = 1_500;

/** One run's time. Run ms count from the start; active ms leave out page gaps: two reads more than 1.5 s apart
    while nothing holds the clock (a frozen or suspended page). Holds cover stage preparation and finalization, whose
    waits are never gaps. */
export class RunClock {
  readonly start: number;
  readonly startedAt: number;
  readonly #clock: Clock;
  #read: number;
  #paused = 0;
  #gap = false;
  #held = false;

  constructor(clock: Clock) {
    this.#clock = clock;
    this.start = this.#read = clock.now();
    this.startedAt = clock.epoch();
  }

  /** Page ms now; notes a gap since the previous read. */
  read(): number {
    const at = this.#clock.now();
    if (!this.#held && at - this.#read > GAP_MS) {
      this.#paused += at - this.#read;
      this.#gap = true;
    }
    return (this.#read = at);
  }

  runMs(): number {
    return this.read() - this.start;
  }

  activeMs(): number {
    return this.read() - this.start - this.#paused;
  }

  /** Whether a gap happened since the last call. */
  takeGap(): boolean {
    const gap = this.#gap;
    this.#gap = false;
    return gap;
  }

  get held(): boolean {
    return this.#held;
  }

  hold(): void {
    this.#held = true;
  }

  /** Page ms now. */
  resume(): number {
    this.#held = false;
    return (this.#read = this.#clock.now());
  }
}
