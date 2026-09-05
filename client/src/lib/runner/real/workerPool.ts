// The measurement worker scripts, and the one way they are stopped.

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

/** One worker owns a whole WebTransport session: its streams cannot be split across workers the way fetch lanes are. */
export function wtTransferWorker(): Worker {
  return new Worker(
    new URL("../workers/wt-transfer-worker.ts", import.meta.url),
    { type: "module" },
  );
}

export function pingWorker(): Worker {
  return new Worker(new URL("../workers/ping-worker.ts", import.meta.url), {
    type: "module",
  });
}
