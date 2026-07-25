// Owns one WebTransport transfer worker: spawn, establish, graceful stop, and
// message routing. The worker owns the session; this class owns the worker, so
// exactly one party ever stops it and a restart cannot orphan a live uploader.
import { wtTransferWorker } from "./workerPool";

export interface WtStartOptions {
  url: string;
  dir: "down" | "up";
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

/** Records of the server's upload feed, relayed verbatim by the worker. */
export type WtProgressRelay =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string };

type WorkerMsg =
  | { type: "established" }
  | { type: "progress"; bytes: number; elapsedMs: number; seq: number }
  | { type: "alive" }
  | { type: "error"; recoverable: boolean; detail: string }
  | { type: "upload-progress"; msg: WtProgressRelay }
  | { type: "stopped" };

export interface WtTransferCallbacks {
  onProgress(bytes: number, elapsedMs: number, seq: number): void;
  onAlive(): void;
  onError(recoverable: boolean, detail: string): void;
  onUploadProgress(msg: WtProgressRelay): void;
}

/** The worker's own establish deadline plus margin for spawn and messaging. */
const ESTABLISH_TIMEOUT_MS = 3500;
/** Covers the worker's finalizing DELETE and its complete-record grace. */
const STOP_GRACE_MS = 2500;

export class WtTransferSession {
  #cb: WtTransferCallbacks;
  #worker: Worker | null = null;
  #stopAck: (() => void) | null = null;
  #established = false;
  /** One session death reaches every lane reader, the accept loop and the
   *  session's own close promise, so the first failure of a generation is the
   *  one reported: the rest describe the same incident and would each cost the
   *  caller a retry. */
  #failed = false;
  #establishTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(cb: WtTransferCallbacks) {
    this.#cb = cb;
  }

  /** Spawn the worker and dial. Establish failures surface through onError
   *  like any lane error, so the caller's retry accounting covers them. */
  start(opts: WtStartOptions): void {
    this.discard();
    this.#established = false;
    this.#failed = false;
    const worker = wtTransferWorker();
    worker.onmessage = (e: MessageEvent<WorkerMsg>): void =>
      this.#onMessage(e.data);
    worker.onerror = (e: ErrorEvent): void =>
      this.#fail(true, e.message || "webtransport worker error");
    worker.postMessage({ type: "start", ...opts });
    this.#worker = worker;
    const armed = worker;
    this.#establishTimer = setTimeout(() => {
      this.#establishTimer = null;
      if (this.#worker === armed && !this.#established)
        this.#fail(true, "webtransport session did not establish");
    }, ESTABLISH_TIMEOUT_MS);
  }

  /** Report the first failure of this generation and swallow its echoes. */
  #fail(recoverable: boolean, detail: string): void {
    if (this.#failed) return;
    this.#failed = true;
    this.#cb.onError(recoverable, detail);
  }

  /** Stop listening to a worker being torn down: messages already queued would
   *  otherwise still deliver, past the point this owner speaks for them. */
  #detach(worker: Worker): void {
    worker.onmessage = null;
    worker.onerror = null;
  }

  measure(seq: number): void {
    this.#worker?.postMessage({ type: "measure", seq });
  }

  /** Graceful stop: the worker finalizes the upload (DELETE, terminal record)
   *  and acks before this resolves and terminates it. */
  stop(): Promise<void> {
    const worker = this.#worker;
    if (!worker) return Promise.resolve();
    this.#worker = null;
    this.#clearEstablishTimer();
    return new Promise((resolve) => {
      const done = (): void => {
        clearTimeout(timer);
        if (this.#stopAck === done) this.#stopAck = null;
        this.#detach(worker);
        worker.terminate();
        resolve();
      };
      const timer = setTimeout(done, STOP_GRACE_MS);
      this.#stopAck = done;
      worker.onmessage = (e: MessageEvent<WorkerMsg>): void => {
        if (e.data.type === "stopped") done();
        // Terminal progress records still count during the grace window.
        else if (e.data.type === "upload-progress")
          this.#cb.onUploadProgress(e.data.msg);
      };
      worker.postMessage({ type: "stop" });
    });
  }

  /** Immediate teardown for aborts: nothing to finalize, kill the session. */
  discard(): void {
    this.#stopAck?.();
    this.#clearEstablishTimer();
    if (this.#worker) {
      this.#detach(this.#worker);
      this.#worker.terminate();
      this.#worker = null;
    }
  }

  #clearEstablishTimer(): void {
    if (this.#establishTimer !== null) {
      clearTimeout(this.#establishTimer);
      this.#establishTimer = null;
    }
  }

  #onMessage(msg: WorkerMsg): void {
    switch (msg.type) {
      case "established":
        this.#established = true;
        this.#clearEstablishTimer();
        break;
      case "progress":
        this.#cb.onProgress(msg.bytes, msg.elapsedMs, msg.seq);
        break;
      case "alive":
        this.#cb.onAlive();
        break;
      case "error":
        this.#fail(msg.recoverable, msg.detail);
        break;
      case "upload-progress":
        this.#cb.onUploadProgress(msg.msg);
        break;
      case "stopped":
        break;
    }
  }
}
