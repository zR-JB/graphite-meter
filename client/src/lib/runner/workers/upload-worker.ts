/* ============================================================
 * The Graphite Meter: upload generate-and-POST worker
 * ============================================================
 * One worker per parallel upload stream. It builds one incompressible Blob pool
 * from CSPRNG bytes, which gzip and br cannot shrink, and POSTs zero-copy slices
 * of it in a loop over whichever HTTP version the origin negotiated. The server
 * drains and counts the bytes; upload-progress-worker.ts relays the
 * authoritative total. This lane saturates.
 * ============================================================ */

import {
  setDebugLogging,
  debugEnabled,
  dlog,
  fmtBytes,
  DebugWindow,
} from "../../debug";
import {
  redirectForCredentials,
  sessionAuthenticationRequired,
  authenticationRequired,
} from "../../request-auth";
import { nextTransferBytes, type SizerCfg } from "./autosize";
import { incompressibleBlock } from "./payload";
import { tuned, DEFAULT_TUNING, type Tuning } from "./tuning";

/** `debug`/`id` drive verbose per-stream logging only. The lane is stopped by
 *  terminating the worker, so there is no shutdown message. */
type InMsg = {
  type: "start";
  url: string;
  debug?: boolean;
  id?: number;
  streams?: number;
  credentials?: RequestCredentials;
  headers?: Record<string, string>;
  tune?: Partial<Tuning>;
};
/** `alive` marks one POST the server drained, proving the lane is live. It
 *  carries no byte count: fetch has no upload-progress events, and the
 *  /upload/progress stream is the authoritative source. `error` restarts a lane. */
type OutMsg =
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "auth-required" };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
let credentials: RequestCredentials = "same-origin";
let headers: Record<string, string> = {};
const post = (m: OutMsg) => ctx.postMessage(m);

/** Pool floor keeps the autosizer useful on constrained devices. */
const MIN_POOL_BYTES = 2 * 1024 * 1024;
/** Reservoir for a device that reports no memory. An absent value is not
 *  evidence of a large device: Chromium reports navigator.deviceMemory, so this
 *  is the Firefox/Safari tier, phones included. */
const UNKNOWN_DEVICE_POOL_BYTES = 128 * 1024 * 1024;
/** A streamed body cycles one small pool: there is no POST size to reserve for. */
const STREAM_POOL_BYTES = 8 * 1024 * 1024;

/** A streamed body reserves nothing: it cycles one small pool instead of sizing
 *  POSTs out of a reservoir, so the device-scaled budget does not apply. */
export function bodyPoolBytes(
  body: Tuning["uploadBody"],
  streams: number,
  deviceMemory?: number,
  totalPoolBytes?: number,
): number {
  return body === "stream"
    ? STREAM_POOL_BYTES
    : uploadPoolBytes(streams, deviceMemory, totalPoolBytes);
}

/** Whether fetch accepts a ReadableStream request body. Chromium only, and it
 *  refuses one over HTTP/1.1, so a caller must also be on h2 or h3. */
export function requestStreamsSupported(): boolean {
  try {
    let asked = false;
    const req = new Request("https://gm.invalid/", {
      method: "POST",
      body: new ReadableStream(),
      get duplex() {
        asked = true;
        return "half";
      },
    } as RequestInit);
    return asked && !req.headers.has("Content-Type");
  } catch {
    return false;
  }
}

/* ---- Closed-loop POST sizing, per worker (see autosize.ts) ---- */
/** The POST target is about ACCURACY: the request/response turnaround sits
 *  inside the server's elapsed-time denominator, so a too-short POST lowers the
 *  measured rate. Interleaved lanes cover each other's turnaround.
 *  maxBytes is the pool size, set on `start`. */
let tuning = tuned();
const sizer: SizerCfg = {
  targetMs: tuning.targetPostMs,
  minBytes: tuning.minPostBytes,
  maxBytes: tuning.minPostBytes,
  alpha: 0.3,
  stepUp: 2,
  stepDown: 0.5,
};

/** Divide the device-scaled total reservoir across the actual lane count. */
export function uploadPoolBytes(
  streams: number,
  deviceMemory?: number,
  totalPoolBytes = tuning.uploadTotalPoolBytes,
): number {
  streams = Math.max(1, streams);
  const reservoir =
    typeof deviceMemory !== "number"
      ? Math.min(UNKNOWN_DEVICE_POOL_BYTES, totalPoolBytes)
      : deviceMemory <= 2
        ? 16 * 1024 * 1024
        : deviceMemory <= 4
          ? 24 * 1024 * 1024
          : totalPoolBytes;
  return Math.max(MIN_POOL_BYTES, Math.floor(reservoir / streams));
}

/** Whether retrying the lane after a non-OK POST is worthwhile. 429 (rate
 *  limited), 413 (too large), 503 (unavailable) and 410 (gone) are terminal for
 *  this run: re-POSTing hammers a server that will not take it. Everything else,
 *  including 500 and any network or abort error, counts as transient. */
export function recoverableStatus(status: number): boolean {
  return !(
    status === 429 ||
    status === 413 ||
    status === 503 ||
    status === 410
  );
}

/** Per-POST controller, supplying `fetch` with a signal. Nothing aborts it: the
 *  lane is stopped by terminating the worker, which drops the request with it. */
let abort: AbortController | null = null;
/** The reused incompressible pool, built on first start. A Blob slice is a view
 *  fetch reads through, which is why Blob is the default. */
let pool: Blob | Uint8Array<ArrayBuffer> | null = null;
/** Byte length of the pool as actually built. */
let poolBytes = 0;
/** Per-lane pool size to build, device-bounded so a phone cannot OOM. Also the
 *  autosizer's upper clamp. */
let poolTargetBytes = tuning.uploadTotalPoolBytes;
/** Bytes the NEXT POST sends, the closed-loop variable. Starts at the minimum
 *  for a fast first sample, then tracks the target times this lane's rate. */
let nextBytes = tuning.minPostBytes;
/** This lane's smoothed throughput (bytes/sec); 0 until the first POST completes. */
let rateEwma = 0;

/** Stream index, tagging debug lines only (`ul-worker#<id>`). */
let streamId = 0;
/** Completed-POST debug window: one step per POST rather than byte-granular,
 *  enough to show whether turnaround leaves the wire idle. */
const dbg = new DebugWindow();

ctx.onmessage = (e: MessageEvent<InMsg>) => {
  const msg = e.data;
  if (msg.type === "start") {
    setDebugLogging(msg.debug ?? false);
    streamId = msg.id ?? 0;
    credentials = msg.credentials ?? "same-origin";
    headers = msg.headers ?? {};
    // Folded to DEFAULT_TUNING unless the build opts into the bench surface
    // (GM_CLIENT_BENCH=1), which also eliminates the merge and msg.tune.
    tuning = __GM_BENCH__ ? tuned(msg.tune) : DEFAULT_TUNING;
    sizer.targetMs = tuning.targetPostMs;
    sizer.minBytes = tuning.minPostBytes;
    const deviceMemory =
      tuning.deviceMemory ??
      (navigator as unknown as { deviceMemory?: number }).deviceMemory;
    poolTargetBytes = bodyPoolBytes(
      tuning.uploadBody,
      msg.streams ?? 1,
      deviceMemory,
      tuning.uploadTotalPoolBytes,
    );
    sizer.maxBytes = poolTargetBytes; // the pool is the size ceiling
    nextBytes = Math.min(tuning.minPostBytes, poolTargetBytes);
    rateEwma = 0;
    dbg.reset();
    void run(msg.url);
  }
};

/** Build the reused pool by repeating one filled block up to poolTargetBytes.
 *  The Blob copies each part into its own backing store, so the construction
 *  heap peaks at ~block + pool. Every POST then slices a view of it. */
function buildPool(): void {
  const wantBlob = tuning.uploadBody === "blob";
  if (
    pool &&
    poolBytes === poolTargetBytes &&
    pool instanceof Blob === wantBlob
  )
    return;
  const block = incompressibleBlock();
  if (wantBlob) {
    const parts: BlobPart[] = [];
    let remaining = poolTargetBytes;
    while (remaining > 0) {
      const take = Math.min(remaining, block.byteLength);
      parts.push(take === block.byteLength ? block : block.subarray(0, take));
      remaining -= take;
    }
    pool = new Blob(parts, { type: "application/octet-stream" });
  } else {
    const bytes = new Uint8Array(new ArrayBuffer(poolTargetBytes));
    for (let off = 0; off < bytes.length; off += block.byteLength)
      bytes.set(
        block.subarray(0, Math.min(block.byteLength, bytes.length - off)),
        off,
      );
    pool = bytes;
  }
  poolBytes = poolTargetBytes;
}

/** A Blob slice is a view fetch reads through; a byte view is copied per POST.
 *  That copy is the cost the Blob path exists to avoid. */
function bodyFor(sentBytes: number): BodyInit {
  return pool instanceof Blob
    ? pool.slice(0, sentBytes)
    : pool!.subarray(0, sentBytes);
}

/** Release the tiny JSON echo so the keep-alive connection serves the next POST:
 *  an unread body pins it and stalls the lane. The POST is already complete, so
 *  a failed release costs at most one connection. */
async function drainForKeepAlive(res: Response): Promise<void> {
  try {
    if (tuning.uploadDrain === "cancel") await res.body?.cancel();
    else await res.arrayBuffer();
  } catch {
    /* the POST already completed */
  }
}

/** One POST whose body never ends, so no request turnaround idles the wire.
 *  `pull` is the backpressure: it fires only when fetch wants more bytes, and
 *  each chunk is a view on the reused pool rather than a copy. */
async function streamPost(url: string): Promise<void> {
  const src = pool as Uint8Array<ArrayBuffer>;
  let off = 0;
  let aliveAt = 0;
  const body = new ReadableStream<Uint8Array>({
    pull(c) {
      const n = Math.min(tuning.writeChunkBytes, src.byteLength);
      if (off + n > src.byteLength) off = 0;
      c.enqueue(src.subarray(off, off + n));
      off += n;
      // A pull is proof of life; the lane only needs it often enough to reset
      // the restart counter, not once per chunk.
      const now = performance.now();
      if (now - aliveAt >= 1000) {
        aliveAt = now;
        post({ type: "alive" });
      }
    },
  });
  abort = new AbortController();
  const res = await fetch(url, {
    method: "POST",
    body,
    duplex: "half",
    signal: abort.signal,
    cache: "no-store",
    headers: { ...headers, "Content-Type": "application/octet-stream" },
    credentials,
    redirect: redirectForCredentials(credentials),
  } as RequestInit);
  if (authenticationRequired(res)) return void post({ type: "auth-required" });
  await drainForKeepAlive(res);
  if (!res.ok)
    post({
      type: "error",
      recoverable: recoverableStatus(res.status),
      detail: `HTTP ${res.status}`,
    });
}

/** Drive the lane for the whole stage: one endless streamed body, or POSTs of
 *  adaptively-sized pool slices in a loop. Mirrors download-worker.ts's
 *  re-fetch loop, and a network error ends the lane (RealBackend restarts it).
 *  Each completed POST resizes the NEXT one. */
async function run(url: string): Promise<void> {
  buildPool();
  if (!pool) return;

  if (tuning.uploadBody === "stream") {
    if (!requestStreamsSupported())
      return void post({
        type: "error",
        recoverable: false,
        detail: "request streaming unsupported",
      });
    try {
      await streamPost(url);
    } catch (err) {
      post({ type: "error", recoverable: true, detail: String(err) });
    }
    return;
  }

  for (;;) {
    abort = new AbortController();
    const sentBytes = nextBytes;
    const postStart = performance.now();
    try {
      const res = await fetch(url, {
        method: "POST",
        body: bodyFor(sentBytes),
        signal: abort.signal,
        cache: "no-store",
        headers: { ...headers, "Content-Type": "application/octet-stream" },
        credentials,
        redirect: redirectForCredentials(credentials),
      });
      if (authenticationRequired(res)) {
        post({ type: "auth-required" });
        return;
      }
      await drainForKeepAlive(res);
      if (!res.ok) {
        post({
          type: "error",
          recoverable: recoverableStatus(res.status),
          detail: `HTTP ${res.status}`,
        });
        return; // RealBackend decides whether to restart this lane
      }
      // The server drained a full slice: the lane is alive. No bytes travel here,
      // /upload/progress is authoritative; this only resets the restart counter.
      post({ type: "alive" });
      ({ bytes: nextBytes, ewma: rateEwma } = nextTransferBytes(
        sentBytes,
        performance.now() - postStart,
        rateEwma,
        sizer,
      ));
      if (debugEnabled()) {
        const window = dbg.add(sentBytes);
        if (window)
          dlog(`ul-worker#${streamId}`, "post-complete", {
            rate: window.rate,
            postSize: fmtBytes(nextBytes),
            window: window.window,
            total: window.total,
            dt: window.dt,
          });
      }
    } catch (err) {
      // A POST that failed on an expired session is an auth failure, not a
      // transport one, so the session is re-checked before the error is reported.
      if (
        credentials === "include" &&
        (await sessionAuthenticationRequired(
          self.location.origin,
          abort.signal,
        ))
      ) {
        post({ type: "auth-required" });
        return;
      }
      post({ type: "error", recoverable: true, detail: String(err) });
      return; // the main thread decides whether to restart this lane
    }
  }
}
