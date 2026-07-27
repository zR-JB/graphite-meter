// Runs one benchmark cell: open the production lanes against a real server,
// discard a warmup window, measure the next one, and report what moved.
//
// This file drives the lanes; it never reads or writes bytes itself. A copied
// read loop would diverge from the shipped one with nothing to catch it, so
// every byte here goes through real/byteLane.ts and the production workers.
import {
  fetchLane,
  sessionLane,
  type ByteLane,
  type LaneEvents,
  type WtProgressRelay,
} from "../src/lib/runner/real/byteLane";
import {
  laneUrl,
  sessionDownloadUrl,
  PER_STREAM_BYTES,
  ROUTES,
  type LaneUrlSpec,
} from "../src/lib/runner/real/backendPure";
import { uploadProgressWorker } from "../src/lib/runner/real/workerPool";

/** Resolution of the within-cell rate series, which yields the stability figure. */
const BUCKET_MS = 200;

export interface CellSpec {
  origin: string;
  dir: "down" | "up";
  transport: "fetch-stream" | "webtransport" | "webtransport-datagram";
  lanes: number;
  warmupMs: number;
  measureMs: number;
  /** Wait for the Alt-Svc upgrade before opening lanes. Firefox reaches h3 only
   *  that way, and the TCP companion carries no transfer routes. */
  bootstrapH3?: boolean;
}

export interface CellResult {
  /** Bytes inside the measured window. Upload counts what the server drained. */
  bytes: number;
  elapsedMs: number;
  /** Per-lane split, so an idle lane is visible rather than averaged away. */
  laneBytes: number[];
  /** Rate samples across the window, each carrying the span it actually took:
   *  a starved main thread delivers the tick late, and assuming BUCKET_MS then
   *  reports a rate the run never reached. */
  buckets: { bytes: number; ms: number }[];
  /** Longest gap between ticks. Well above BUCKET_MS means the page could not
   *  keep up, which caps what any lane count can deliver. */
  maxTickMs: number;
  errors: string[];
}

const sleep = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

/** The server's running total is authoritative for upload. The first record of
 *  the measured window is the baseline, matching what the app counts. */
class ServerTotal {
  #baseline = -1;
  bytes = 0;
  measuring = false;

  accept(n: number): void {
    if (!this.measuring) {
      this.#baseline = n;
      return;
    }
    if (this.#baseline < 0) this.#baseline = n;
    this.bytes = Math.max(this.bytes, n - this.#baseline);
  }

  beginMeasure(): void {
    this.measuring = true;
  }
}

/** Polls the bootstrap probe until the server reports the connection as h3.
 *  The server's own view is authoritative; nextHopProtocol is masked cross-origin. */
async function bootstrapH3(origin: string): Promise<boolean> {
  const url = `${origin}${ROUTES.probe}`;
  for (let i = 0; i < 24; i++) {
    const proto = await fetch(url, { cache: "no-store" })
      .then((r) => r.json() as Promise<{ protocolNegotiated?: string }>)
      .then((p) => p.protocolNegotiated ?? "")
      .catch(() => "");
    if (proto === "h3") return true;
    await sleep(250);
  }
  return false;
}

async function mintUploadId(origin: string): Promise<string> {
  const res = await fetch(`${origin}${ROUTES.uploadSession}`, {
    method: "POST",
    cache: "no-store",
  });
  if (!res.ok) throw new Error(`upload session: HTTP ${res.status}`);
  return ((await res.json()) as { uploadId: string }).uploadId;
}

/** The fetch upload feed, which the app opens before any POST lane so the
 *  server's counter is already running when bytes start. */
function openProgressFeed(
  origin: string,
  uploadId: string,
  total: ServerTotal,
  errors: string[],
): { worker: Worker; open: Promise<void> } {
  const worker = uploadProgressWorker();
  let resolveOpen: () => void;
  const open = new Promise<void>((resolve) => (resolveOpen = resolve));
  worker.onmessage = (e: MessageEvent<WtProgressRelay>): void => {
    const msg = e.data;
    if (msg.type === "open") resolveOpen();
    else if (msg.type === "bytes" || msg.type === "complete")
      total.accept(msg.n);
    else if (msg.type === "fatal") errors.push(`progress: ${msg.detail}`);
  };
  worker.postMessage({
    type: "start",
    url: `${origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(uploadId)}`,
  });
  return { worker, open };
}

export async function runCell(spec: CellSpec): Promise<CellResult> {
  const errors: string[] = [];
  const laneBytes: number[] = new Array(spec.lanes).fill(0);
  const total = new ServerTotal();
  let clientBytes = 0;
  let measuring = false;

  const rides = spec.transport !== "fetch-stream";
  const session = rides
    ? {
        origin: spec.origin,
        uploadPath: ROUTES.wtUpload,
        downloadPath: ROUTES.wtDownload,
        datagrams: spec.transport === "webtransport-datagram",
      }
    : null;
  const urls: LaneUrlSpec = {
    dir: spec.dir,
    base: spec.origin,
    downloadPath: ROUTES.download,
    uploadPath: ROUTES.upload,
    cbSeed: `bench${Math.round(performance.now())}`,
    bytes: PER_STREAM_BYTES,
    // The lane sweep measures the long-stream lane, which is what the app runs.
    chunkDownload: false,
    session,
  };

  const events = (i: number): LaneEvents => ({
    onProgress(bytes) {
      if (!measuring) return;
      clientBytes += bytes;
      laneBytes[i] = (laneBytes[i] ?? 0) + bytes;
    },
    onAlive() {},
    onError(_recoverable, detail) {
      errors.push(`lane ${i}: ${detail}`);
    },
    // A session upload relays the server's feed over the same session.
    onUploadProgress(msg) {
      if (msg.type === "bytes" || msg.type === "complete") total.accept(msg.n);
      else if (msg.type === "fatal") errors.push(`progress: ${msg.detail}`);
    },
    onAuthRequired() {
      errors.push("authentication required");
    },
  });

  // Bootstrapping before the mint keeps every request of the cell on h3. A cell
  // that never upgraded would measure the TCP companion, so it is an error.
  if (spec.bootstrapH3 && !(await bootstrapH3(spec.origin)))
    errors.push("h3 bootstrap: never negotiated h3");

  const uploadId = spec.dir === "up" ? await mintUploadId(spec.origin) : "";
  // Only a fetch upload needs its own feed worker; a session carries its own.
  const feed =
    spec.dir === "up" && !rides
      ? openProgressFeed(spec.origin, uploadId, total, errors)
      : null;
  if (feed) await feed.open;

  const lanes: ByteLane[] = [];
  if (session) {
    const opts = {
      url:
        spec.dir === "down"
          ? sessionDownloadUrl(session, PER_STREAM_BYTES, spec.lanes)
          : laneUrl(urls, 0, uploadId),
      dir: spec.dir,
      lanes: spec.lanes,
      datagrams: session.datagrams,
      progressUrl:
        spec.dir === "up"
          ? `${spec.origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(uploadId)}`
          : undefined,
    };
    lanes.push(sessionLane(opts, events(0)));
  } else {
    for (let i = 0; i < spec.lanes; i++)
      lanes.push(
        fetchLane(
          {
            dir: spec.dir,
            url: laneUrl(urls, i, uploadId),
            lanes: spec.lanes,
            index: i,
            credentials: "same-origin",
            chunk: urls.chunkDownload,
            debug: false,
          },
          events(i),
        ),
      );
  }
  for (const lane of lanes) lane.start();

  await sleep(spec.warmupMs);

  // The measure epoch is what separates warmup from measurement: a download
  // report carrying the old seq is discarded by the worker's own accounting.
  clientBytes = 0;
  laneBytes.fill(0);
  measuring = true;
  total.beginMeasure();
  for (const lane of lanes) lane.measure(1);

  const buckets: { bytes: number; ms: number }[] = [];
  const readTotal = (): number =>
    spec.dir === "up" ? total.bytes : clientBytes;
  let last = readTotal();
  const startedAt = performance.now();
  let lastAt = startedAt;
  let maxTickMs = 0;
  const ticker = setInterval(() => {
    const at = performance.now();
    const n = readTotal();
    buckets.push({ bytes: n - last, ms: at - lastAt });
    maxTickMs = Math.max(maxTickMs, at - lastAt);
    last = n;
    lastAt = at;
  }, BUCKET_MS);

  await sleep(spec.measureMs);
  clearInterval(ticker);
  const elapsedMs = performance.now() - startedAt;

  await Promise.all(lanes.map((lane) => lane.stop()));
  if (feed) {
    feed.worker.postMessage({ type: "stop" });
    feed.worker.terminate();
  }
  if (spec.dir === "up")
    await fetch(
      `${spec.origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(uploadId)}`,
      { method: "DELETE", cache: "no-store" },
    ).catch(() => {});

  return {
    bytes: readTotal(),
    elapsedMs,
    laneBytes,
    buckets,
    maxTickMs,
    errors,
  };
}

declare global {
  interface Window {
    __gmBench: {
      run(spec: CellSpec): Promise<CellResult>;
    };
  }
}

window.__gmBench = { run: runCell };
document.getElementById("state")!.textContent = "ready";
