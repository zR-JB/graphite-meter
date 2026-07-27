// One seam for the byte lanes. A transfer direction drives lanes through this
// interface and never learns which transport is underneath.
import type { FlowDirection } from "../contract";
import type { ProgressEvent } from "../workers/progressFeed";
import {
  downloadWorker,
  stopWorker,
  uploadWorker,
  wtTransferWorker,
} from "./workerPool";
import {
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  STOP_GRACE_MS,
} from "./budgets";

/** Records of the server's upload feed, relayed verbatim by a session lane. */
export type { ProgressEvent as WtProgressRelay };

/** What every lane reports, whatever carries its bytes. */
export interface LaneEvents {
  onProgress(bytes: number, elapsedMs?: number, seq?: number): void;
  /** A unit of work completed without a byte count, which upload lanes report. */
  onAlive(): void;
  onError(recoverable: boolean, detail: string): void;
  onUploadProgress(msg: ProgressEvent): void;
  onAuthRequired(): void;
}

export interface ByteLane {
  start(): void;
  /** Enter the measured window under this epoch; reports before it are warmup. */
  measure(seq: number): void;
  /** Finish cleanly, letting a lane close its transport and finalize. */
  stop(): Promise<void>;
  /** Drop immediately, for an abort or a restart. */
  discard(): void;
}

export interface FetchLaneOptions {
  dir: FlowDirection;
  url: string;
  lanes: number;
  index: number;
  headers?: Record<string, string>;
  credentials: RequestCredentials;
  chunk: boolean;
  debug: boolean;
}

export interface SessionLaneOptions {
  url: string;
  dir: FlowDirection;
  lanes: number;
  datagrams: boolean;
  /** Token mint the worker calls before dialing, when authentication is on. */
  mint?: {
    url: string;
    headers?: Record<string, string>;
    credentials?: RequestCredentials;
  };
  progressUrl?: string;
  headers?: Record<string, string>;
  credentials?: RequestCredentials;
}

type WorkerMsg =
  | { type: "established" }
  | { type: "progress"; bytes: number; elapsedMs?: number; seq?: number }
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "upload-progress"; msg: ProgressEvent }
  | { type: "auth-required" }
  | { type: "stopped" };

/** One fetch request per lane: the worker script owns its own retries within
 *  one request, and a dropped lane is restarted by the caller. */
export function fetchLane(
  opts: FetchLaneOptions,
  events: LaneEvents,
): ByteLane {
  let worker: Worker | null = null;
  return {
    start(): void {
      const w = opts.dir === "down" ? downloadWorker() : uploadWorker();
      w.onmessage = (e: MessageEvent<WorkerMsg>): void => {
        const msg = e.data;
        switch (msg.type) {
          case "progress":
            events.onProgress(msg.bytes, msg.elapsedMs, msg.seq);
            break;
          case "alive":
            events.onAlive();
            break;
          case "error":
            events.onError(msg.recoverable, msg.detail);
            break;
          case "auth-required":
            events.onAuthRequired();
            break;
        }
      };
      w.onerror = (e: ErrorEvent): void =>
        events.onError(true, e.message || "worker error");
      // `debug`/`id` only drive the worker's own verbose per-stream logging.
      w.postMessage({
        type: "start",
        url: opts.url,
        debug: opts.debug,
        id: opts.index,
        streams: opts.lanes,
        credentials: opts.credentials,
        headers: opts.headers,
        chunk: opts.chunk,
      });
      worker = w;
    },
    measure(seq: number): void {
      worker?.postMessage({ type: "measure", seq });
    },
    stop(): Promise<void> {
      if (worker) stopWorker(worker);
      worker = null;
      return Promise.resolve();
    },
    discard(): void {
      worker?.terminate();
      worker = null;
    },
  };
}

/** One worker owns a whole WebTransport session: its streams cannot be split
 *  across workers the way fetch lanes are, so the session is one lane here. */
export function sessionLane(
  opts: SessionLaneOptions,
  events: LaneEvents,
): ByteLane {
  let worker: Worker | null = null;
  let established = false;
  // One session death reaches every lane reader, the accept loop and the close
  // promise, so only the first failure of a generation is reported.
  let failed = false;
  let establishTimer: ReturnType<typeof setTimeout> | null = null;
  let stopAck: (() => void) | null = null;

  const clearEstablishTimer = (): void => {
    if (establishTimer !== null) clearTimeout(establishTimer);
    establishTimer = null;
  };
  const fail = (recoverable: boolean, detail: string): void => {
    if (failed) return;
    failed = true;
    events.onError(recoverable, detail);
  };
  // Messages already queued would otherwise still deliver, past the point this
  // lane speaks for them.
  const detach = (w: Worker): void => {
    w.onmessage = null;
    w.onerror = null;
  };

  const onMessage = (msg: WorkerMsg): void => {
    switch (msg.type) {
      case "established":
        established = true;
        clearEstablishTimer();
        break;
      case "progress":
        events.onProgress(msg.bytes, msg.elapsedMs, msg.seq);
        break;
      case "alive":
        events.onAlive();
        break;
      case "error":
        fail(msg.recoverable, msg.detail);
        break;
      case "upload-progress":
        events.onUploadProgress(msg.msg);
        break;
      case "auth-required":
        events.onAuthRequired();
        break;
    }
  };

  return {
    start(): void {
      this.discard();
      established = false;
      failed = false;
      const w = wtTransferWorker();
      w.onmessage = (e: MessageEvent<WorkerMsg>): void => onMessage(e.data);
      w.onerror = (e: ErrorEvent): void =>
        fail(true, e.message || "webtransport worker error");
      w.postMessage({ type: "start", ...opts });
      worker = w;
      establishTimer = setTimeout(() => {
        establishTimer = null;
        if (worker === w && !established)
          fail(true, "webtransport session did not establish");
      }, ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS);
    },
    measure(seq: number): void {
      worker?.postMessage({ type: "measure", seq });
    },
    // The worker finalizes the upload and acks before this resolves.
    stop(): Promise<void> {
      const w = worker;
      if (!w) return Promise.resolve();
      worker = null;
      clearEstablishTimer();
      return new Promise((resolve) => {
        const done = (): void => {
          clearTimeout(timer);
          if (stopAck === done) stopAck = null;
          detach(w);
          w.terminate();
          resolve();
        };
        const timer = setTimeout(done, STOP_GRACE_MS);
        stopAck = done;
        w.onmessage = (e: MessageEvent<WorkerMsg>): void => {
          if (e.data.type === "stopped") done();
          else if (e.data.type === "progress")
            events.onProgress(e.data.bytes, e.data.elapsedMs, e.data.seq);
          // Terminal progress records still count during the grace window.
          else if (e.data.type === "upload-progress")
            events.onUploadProgress(e.data.msg);
        };
        w.postMessage({ type: "stop" });
      });
    },
    discard(): void {
      stopAck?.();
      clearEstablishTimer();
      if (worker) {
        detach(worker);
        worker.terminate();
        worker = null;
      }
    },
  };
}
