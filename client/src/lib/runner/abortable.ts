/** Bound the wait even when an adapter ignores the signal; the operation still owns resource cleanup. */
export function abortable<T>(
  promise: Promise<T>,
  signal: AbortSignal,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason);
    signal.addEventListener("abort", abort, { once: true });
    promise
      .then(resolve, reject)
      .finally(() => signal.removeEventListener("abort", abort));
    if (signal.aborted) abort();
  });
}

export async function withinBudget<T>(
  owner: AbortSignal,
  milliseconds: number,
  run: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  owner.throwIfAborted();
  const deadline = new AbortController();
  const signal = AbortSignal.any([owner, deadline.signal]);
  const timer = setTimeout(
    () =>
      deadline.abort(
        new DOMException("Connection check timed out", "TimeoutError"),
      ),
    milliseconds,
  );
  try {
    const value = await abortable(run(signal), signal);
    signal.throwIfAborted();
    return value;
  } finally {
    clearTimeout(timer);
  }
}

export async function abortableDelay(
  milliseconds: number,
  signal: AbortSignal,
): Promise<void> {
  signal.throwIfAborted();
  let timer: ReturnType<typeof setTimeout>;
  const waiting = new Promise<void>((resolve) => {
    timer = setTimeout(resolve, milliseconds);
  });
  try {
    await abortable(waiting, signal);
  } finally {
    clearTimeout(timer!);
  }
}
