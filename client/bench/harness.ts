// Runs one benchmark cell against a real server, measuring production lanes after warmup.
// This driver never reads or writes bytes itself; all traffic goes through production workers.
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
  /** Wait for Alt-Svc h3 upgrade before opening lanes; the TCP companion has no transfer routes. */
  bootstrapH3?: boolean;
}

export interface CellResult {
  /** Bytes inside the measured window. Upload counts what the server drained. */
  bytes: number;
  elapsedMs: number;
  /** Per-lane split, so an idle lane is visible rather than averaged away. */
  laneBytes: number[];
  /** Rate samples carry actual spans so late ticks do not report an unattained rate. */
  buckets: { bytes: number; ms: number }[];
  /** Longest tick gap; a value above BUCKET_MS indicates a page-stalled run. */
  maxTickMs: number;
  errors: string[];
}

const sleep = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

/** The server total is authoritative for upload, with the window's first record as baseline. */
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

/** Polls until the server reports h3; its view is authoritative when nextHopProtocol is masked. */
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

/** Opens the fetch upload feed before POST lanes so the server counter runs before bytes start. */
function openProgressFeed(
  origin: string,
  uploadId: string,
  total: ServerTotal,
  errors: string[],
): { worker: Worker; open: Promise<boolean> } {
  const worker = uploadProgressWorker();
  let resolveOpen!: (opened: boolean) => void;
  const open = new Promise<boolean>((resolve) => (resolveOpen = resolve));
  let opening = true;
  worker.onmessage = (e: MessageEvent<WtProgressRelay>): void => {
    const msg = e.data;
    if (msg.type === "open") {
      opening = false;
      resolveOpen(true);
    } else if (msg.type === "bytes" || msg.type === "complete")
      total.accept(msg.n);
    else if (msg.type === "fatal") {
      errors.push(`progress: ${msg.detail}`);
      if (opening) {
        opening = false;
        resolveOpen(false);
      }
    }
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

  // Bootstrap before minting so every cell request uses h3; otherwise it measures the TCP companion.
  if (spec.bootstrapH3 && !(await bootstrapH3(spec.origin)))
    errors.push("h3 bootstrap: never negotiated h3");

  const uploadId = spec.dir === "up" ? await mintUploadId(spec.origin) : "";
  // Only a fetch upload needs its own feed worker; a session carries its own.
  const feed =
    spec.dir === "up" && !rides
      ? openProgressFeed(spec.origin, uploadId, total, errors)
      : null;
  if (feed && !(await feed.open)) {
    feed.worker.terminate();
    await fetch(
      `${spec.origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(uploadId)}`,
      { method: "DELETE", cache: "no-store" },
    ).catch(() => {});
    return {
      bytes: 0,
      elapsedMs: 0,
      laneBytes,
      buckets: [],
      maxTickMs: 0,
      errors,
    };
  }

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
            credentials: "same-origin",
            chunk: urls.chunkDownload,
          },
          events(i),
        ),
      );
  }
  for (const lane of lanes) lane.start();

  await sleep(spec.warmupMs);

  // The measure epoch separates warmup; workers discard download reports carrying its old sequence.
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
