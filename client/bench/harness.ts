// Runs one benchmark cell against a real server, measuring production lanes after warmup.
// Byte lanes and upload accounting use the production transport implementations.
import {
  laneWorker,
  openLane,
  uploadFeed,
  type Lane,
  type WorkerMsg,
} from "../src/lib/runner/transport";
import { laneUrl, PER_STREAM_BYTES, ROUTES } from "../src/lib/runner/paths";

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
): { dispose(): void; open: Promise<boolean> } {
  let resolveOpen!: (opened: boolean) => void;
  const open = new Promise<boolean>((resolve) => (resolveOpen = resolve));
  let opening = true;
  const feed = uploadFeed({
    url: `${origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(uploadId)}`,
    csrf: {},
    credentials: "same-origin",
    onEvent(msg) {
      if (msg.type === "open") {
        opening = false;
        resolveOpen(true);
      } else if (msg.type === "bytes" || msg.type === "complete")
        total.accept(msg.n);
      else if (msg.type === "fatal" || msg.type === "auth-required") {
        errors.push(
          `progress: ${msg.type === "fatal" ? msg.detail : "authentication required"}`,
        );
        if (opening) {
          opening = false;
          resolveOpen(false);
        }
      }
    },
  });
  return { dispose: feed.dispose, open };
}

export async function runCell(spec: CellSpec): Promise<CellResult> {
  const errors: string[] = [];
  const laneBytes: number[] = new Array(spec.lanes).fill(0);
  const total = new ServerTotal();
  let clientBytes = 0;
  let measuring = false;

  const rides = spec.transport !== "fetch-stream";
  const datagrams = spec.transport === "webtransport-datagram";
  const urls = {
    dir: spec.dir,
    base: spec.origin,
    cbSeed: `bench${Math.round(performance.now())}`,
  };

  const events =
    (i: number) =>
    (msg: WorkerMsg): void => {
      if (msg.type === "progress" && measuring) {
        clientBytes += msg.bytes;
        laneBytes[i] = (laneBytes[i] ?? 0) + msg.bytes;
      } else if (msg.type === "error") errors.push(`lane ${i}: ${msg.detail}`);
      else if (msg.type === "auth-required")
        errors.push("authentication required");
      // A session upload relays the server's feed over the same session.
      else if (msg.type === "upload-progress") {
        if (msg.msg.type === "bytes" || msg.msg.type === "complete")
          total.accept(msg.msg.n);
        else if (msg.msg.type === "fatal")
          errors.push(`progress: ${msg.msg.detail}`);
      }
    };

  // Bootstrap before minting so every cell request uses h3; otherwise it measures the TCP companion.
  if (spec.bootstrapH3 && !(await bootstrapH3(spec.origin)))
    errors.push("h3 bootstrap: never negotiated h3");

  const uploadId = spec.dir === "up" ? await mintUploadId(spec.origin) : "";
  // Fetch uploads use the direct HTTP feed; a session carries its own.
  const feed =
    spec.dir === "up" && !rides
      ? openProgressFeed(spec.origin, uploadId, total, errors)
      : null;
  if (feed && !(await feed.open)) {
    feed.dispose();
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

  const lanes: Lane[] = [];
  if (rides) {
    const query =
      spec.dir === "down"
        ? `bytes=${PER_STREAM_BYTES}&${datagrams ? "datagrams=1" : `streams=${spec.lanes}`}`
        : `id=${encodeURIComponent(uploadId)}${datagrams ? "&datagrams=1" : ""}`;
    const path = spec.dir === "down" ? ROUTES.wtDownload : ROUTES.wtUpload;
    const start = {
      url: `${spec.origin}${path}?${query}`,
      dir: spec.dir,
      lanes: spec.lanes,
      datagrams,
      progressUrl:
        spec.dir === "up"
          ? `${spec.origin}${ROUTES.uploadProgress}?id=${encodeURIComponent(uploadId)}`
          : undefined,
    };
    lanes.push(openLane(laneWorker("wt"), start, true, events(0)));
  } else {
    for (let i = 0; i < spec.lanes; i++)
      lanes.push(
        openLane(
          laneWorker(spec.dir === "down" ? "download" : "upload"),
          {
            url: laneUrl(urls, i, uploadId),
            streams: spec.lanes,
            credentials: "same-origin",
          },
          false,
          events(i),
        ),
      );
  }

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
    feed.dispose();
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
