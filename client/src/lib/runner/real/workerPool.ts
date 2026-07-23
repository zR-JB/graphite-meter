// The four measurement worker scripts and their shared shutdown handshake.
// Each `new URL(..., import.meta.url)` is a build-time bundling anchor — the
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

/** Ask a worker to close its transport, then terminate it. */
export function stopWorker(w: Worker): void {
  w.postMessage({ type: "stop" });
  w.terminate();
}
