// A server's transfer workers, kept across stages and runs, so a run after the first starts none; a sweep ends idle ones.
import { workerScript } from "../../lib/trusted";
import type { WorkerPool } from "../types";

export function createWorkerPool(): WorkerPool & {
  drain(): void;
  sweep(): void;
  close(): void;
} {
  const idle = new Map<string, Worker[]>();
  let closed = false;
  let taken = false;
  const drain = () => {
    for (const workers of idle.values()) for (const worker of workers.splice(0)) worker.terminate();
  };
  return {
    take(script) {
      taken = true;
      return idle.get(script)?.pop() ?? new Worker(workerScript(script), { type: "module" });
    },
    give(script, worker) {
      if (closed) return worker.terminate();
      const workers = idle.get(script) ?? [];
      idle.set(script, workers);
      workers.push(worker);
    },
    drain,
    sweep() {
      if (!taken) drain();
      taken = false;
    },
    close() {
      closed = true;
      drain();
    },
  };
}
