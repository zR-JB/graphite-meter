// The run's clock keeps its pace in a hidden tab: page timers there slow to one wake a minute after a few
// minutes, which would split a long run's evidence at every wake. Outside a page, plain timers serve.
let worker: Worker | null | undefined;
let next = 0;
const due = new Map<number, () => void>();

function timerWorker(): Worker | null {
  if (worker !== undefined) return worker;
  worker = null;
  if (typeof document === "undefined" || typeof Worker === "undefined")
    return worker;
  try {
    worker = new Worker(new URL("./workers/timer-worker.ts", import.meta.url), {
      type: "module",
    });
    worker.onmessage = ({ data: id }: MessageEvent<number>) => {
      const run = due.get(id);
      due.delete(id);
      run?.();
    };
  } catch {
    worker = null;
  }
  return worker;
}

/** Runs `run` after `ms`; the returned function cancels it. */
export function after(ms: number, run: () => void): () => void {
  const host = timerWorker();
  if (!host) {
    const timer = setTimeout(run, ms);
    return () => clearTimeout(timer);
  }
  const id = ++next;
  due.set(id, run);
  host.postMessage({ id, ms });
  return () => {
    if (due.delete(id)) host.postMessage({ id, ms: null });
  };
}
