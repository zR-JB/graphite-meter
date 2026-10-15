// Waits for the lanes' owners, bounded by a signal, on the page's worker-backed timers: a hidden page keeps its
// recovery pace.
import { pageClock } from "../run/clock";

/** Resolves after `ms`, or at once when `signal` aborts; the timer never outlives either. */
export function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const done = () => {
      cancel();
      signal.removeEventListener("abort", done);
      resolve();
    };
    const cancel = pageClock.after(ms, done);
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
