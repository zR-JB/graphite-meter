// One seam for the byte lanes.
import type { FlowDirection, RecoveryCause } from "../contract";
import type { ProgressEvent } from "../workers/progressFeed";
import { downloadWorker, uploadWorker, wtTransferWorker } from "./workerPool";
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
  /** A locally timed upload completion, usable only for visual presentation. */
  onAlive(bytes?: number, elapsedMs?: number): void;
  onError(recoverable: boolean, detail: string, cause?: RecoveryCause): void;
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

interface FetchLaneOptions {
  dir: FlowDirection;
  url: string;
  lanes: number;
  headers?: Record<string, string>;
  credentials: RequestCredentials;
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
  | { type: "alive"; bytes?: number; elapsedMs?: number }
  | {
      type: "error";
      recoverable: boolean;
      detail: string;
      cause?: RecoveryCause;
    }
  | { type: "upload-progress"; msg: ProgressEvent }
  | { type: "auth-required" }
  | { type: "stopped" };

function dispatchWorkerMessage(
  msg: WorkerMsg,
  events: LaneEvents,
  onError: LaneEvents["onError"] = events.onError,
  onEstablished: () => void = () => {},
): void {
  switch (msg.type) {
    case "established":
      onEstablished();
      break;
    case "progress":
      events.onProgress(msg.bytes, msg.elapsedMs, msg.seq);
      break;
    case "alive":
      events.onAlive(msg.bytes, msg.elapsedMs);
      break;
    case "error":
      onError(msg.recoverable, msg.detail, msg.cause);
      break;
    case "upload-progress":
      events.onUploadProgress(msg.msg);
      break;
    case "auth-required":
      events.onAuthRequired();
      break;
    case "stopped":
      break;
  }
}

/* One fetch worker per lane; discarding the lane revokes its callbacks immediately. */
export function fetchLane(
  opts: FetchLaneOptions,
  events: LaneEvents,
): ByteLane {
  let worker: Worker | null = null;
  return {
    start(): void {
      this.discard();
      const w = opts.dir === "down" ? downloadWorker() : uploadWorker();
      worker = w;
      w.onmessage = (e: MessageEvent<WorkerMsg>): void => {
        dispatchWorkerMessage(e.data, events);
      };
      w.onerror = (e: ErrorEvent): void =>
        events.onError(true, e.message || "worker error");
      w.postMessage({
        type: "start",
        url: opts.url,
        streams: opts.lanes,
        credentials: opts.credentials,
        headers: opts.headers,
      });
    },
    measure(seq: number): void {
      worker?.postMessage({ type: "measure", seq });
    },
    stop(): Promise<void> {
      this.discard();
      return Promise.resolve();
    },
    discard(): void {
      if (!worker) return;
      worker.onmessage = null;
      worker.onerror = null;
      worker.terminate();
      worker = null;
    },
  };
}

/* One worker owns a whole WebTransport session and all of its streams. */
export function sessionLane(
  opts: SessionLaneOptions,
  events: LaneEvents,
): ByteLane {
  let worker: Worker | null = null;
  let established = false;
  // Readers, the accept loop and the close promise can report the same session failure.
  let failed = false;
  let establishTimer: ReturnType<typeof setTimeout> | null = null;
  let stopAck: (() => void) | null = null;

  const clearEstablishTimer = (): void => {
    if (establishTimer !== null) clearTimeout(establishTimer);
    establishTimer = null;
  };
  const fail = (
    recoverable: boolean,
    detail: string,
    cause?: RecoveryCause,
  ): void => {
    if (failed) return;
    failed = true;
    events.onError(recoverable, detail, cause);
  };
  // Messages already queued would otherwise still deliver, past the point this lane speaks for them.
  const detach = (w: Worker): void => {
    w.onmessage = null;
    w.onerror = null;
  };

  const onMessage = (msg: WorkerMsg): void => {
    dispatchWorkerMessage(msg, events, fail, () => {
      established = true;
      clearEstablishTimer();
    });
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
