// The measurement worker scripts, and the one way they are stopped.
// Each `new URL(..., import.meta.url)` is a build-time bundling anchor: the
// specifier must stay a literal.

/** Every worker reports this when the server rejects it as unauthenticated. */
export type AuthRequiredMsg = { type: "auth-required" };

export function downloadWorker(): Worker {
  return new Worker(new URL("../workers/download-worker.ts", import.meta.url), {
    type: "module",
  });
}

export function uploadWorker(): Worker {
  return new Worker(new URL("../workers/upload-worker.ts", import.meta.url), {
    type: "module",
  });
}

/** One worker owns a whole WebTransport session: its streams cannot be split
 *  across workers the way fetch lanes are. */
export function wtTransferWorker(): Worker {
  return new Worker(
    new URL("../workers/wt-transfer-worker.ts", import.meta.url),
    { type: "module" },
  );
}

export function uploadProgressWorker(): Worker {
  return new Worker(
    new URL("../workers/upload-progress-worker.ts", import.meta.url),
    { type: "module" },
  );
}

export function pingWorker(): Worker {
  return new Worker(new URL("../workers/ping-worker.ts", import.meta.url), {
    type: "module",
  });
}

/** Terminate a worker. These workers own nothing the main thread waits on, and
 *  terminate() drops the transport along with any message still queued, so
 *  there is no shutdown handshake to run first. */
export function stopWorker(worker: Worker): void {
  worker.terminate();
}
