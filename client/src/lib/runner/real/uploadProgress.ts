// The server-authoritative upload meter: the /upload/progress control channel
// and the server byte/time counters derived from it.
import type { CoreHost } from "../core";
import type { PhaseActivity } from "../contract";
import type { FetchThroughputTarget } from "../../api/endpoints";
import { authEnabled, csrfHeader, redirectToLogin } from "../../auth";
import { uploadProgressWorker, type AuthRequiredMsg } from "./workerPool";

/** Upload-progress worker → channel messages. `bytes`/`complete` carry the
 *  SERVER's cumulative drained count `n` and elapsed clock `t` (ns) it was
 *  sampled at — the SOLE upload byte source. Rate is derived over server time
 *  (Δn / Δt), so the live curve and the totals headline are both immune to local
 *  tick/arrival jitter. stall/resume bracket control-channel recovery. */
type ProgressOutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string }
  | { type: "stall"; detail: string }
  | { type: "resume" };

const PROGRESS_ESTABLISH_TIMEOUT_MS = 3500;
const PROGRESS_BYE_GRACE_MS = 1000;

/** The upload lane fields this channel reads and marks. */
export interface UploadProgressLane {
  stage: PhaseActivity["stage"];
  measuring: boolean;
  stageSawBytes: boolean;
}

export interface UploadProgressDeps {
  host: () => CoreHost;
  target: () => FetchThroughputTarget | null;
  headers: () => HeadersInit | undefined;
  /** The "up" lane, or undefined once the stage was torn down. */
  lane: () => UploadProgressLane | undefined;
  transferActive: () => boolean;
  discardTransfer: () => void;
  setLaneStalled: (stalled: boolean, detail?: string) => void;
}

export class UploadProgressChannel {
  #deps: UploadProgressDeps;
  #worker: Worker | null = null;
  #ready: { finish: (ready: boolean) => void } | null = null;
  #done: (() => void) | null = null;
  /** Latest cumulative server byte count and previous measured snapshot. */
  #srvN = 0;
  #srvPrevN = 0; // cumulative at the last delta fed into the live curve
  #srvPrevT = 0; // server elapsed ns of that last delta — the live-curve denominator
  #srvHaveStart = false;

  constructor(deps: UploadProgressDeps) {
    this.#deps = deps;
  }

  /** Establish the server-authoritative upload progress stream before starting
   *  POST lanes. Upload cannot be measured honestly without this channel. */
  prime(stage: PhaseActivity["stage"], uploadId: string): Promise<boolean> {
    this.#resetCounters();

    if (!uploadId) return Promise.resolve(false);
    const host = this.#deps.host();
    const target = this.#deps.target();
    const progressRoute = target?.routes.uploadProgress;
    if (!target || !progressRoute) {
      host.failStage(
        stage,
        "transport-unavailable",
        "selected throughput target has no upload progress route",
      );
      return Promise.resolve(false);
    }

    const url = `${target.origin}${progressRoute}?id=${encodeURIComponent(uploadId)}`;
    const w = uploadProgressWorker();
    const ready = new Promise<boolean>((resolve) => {
      const finish = (established: boolean): void => {
        if (this.#ready?.finish !== finish) return;
        clearTimeout(timer);
        this.#ready = null;
        resolve(established);
      };
      const timer = setTimeout(() => {
        host.failStage(
          stage,
          "connection-lost",
          "upload progress channel could not be established",
        );
        finish(false);
      }, PROGRESS_ESTABLISH_TIMEOUT_MS);
      this.#ready = { finish };
    });
    w.onmessage = (e: MessageEvent<ProgressOutMsg>): void => {
      if (e.data.type === "open") this.#ready?.finish(true);
      this.#onMessage(e.data);
    };
    w.onerror = (): void => {
      /* the worker owns reconnect; a hard worker error just means no server bytes
       * until it recovers, which the stall watchdog already covers. */
    };
    w.postMessage({
      type: "start",
      url,
      headers: this.#deps.headers(),
      csrf: csrfHeader(),
      credentials: authEnabled ? "include" : "same-origin",
    });
    this.#worker = w;
    return ready;
  }

  /** Open the measured window: the first progress frame after this boundary
   *  becomes the upload baseline, excluding warmup bytes and time together. */
  beginMeasure(): void {
    this.#srvHaveStart = false;
    this.#srvPrevN = this.#srvN;
  }

  /** Stop the progress worker after the POST lanes. It explicitly finalizes the
   *  session with DELETE and lets the stream receive the terminal complete record. */
  teardown(finalize: boolean): Promise<void> {
    this.#ready?.finish(false);
    this.#ready = null;
    const w = this.#worker;
    if (!w) return Promise.resolve();
    const worker = w;
    if (!finalize) {
      if (this.#done) {
        this.#done();
        return Promise.resolve();
      }
      this.#worker = null;
      worker.terminate();
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      const timer = setTimeout(done, PROGRESS_BYE_GRACE_MS);
      const self = this;
      function done(): void {
        clearTimeout(timer);
        if (self.#done === done) self.#done = null;
        if (self.#worker === worker) self.#worker = null;
        worker.terminate();
        resolve();
      }
      this.#done = done;
      worker.postMessage({ type: "stop" });
    });
  }

  #resetCounters(): void {
    this.#srvN = 0;
    this.#srvPrevN = 0;
    this.#srvPrevT = 0;
    this.#srvHaveStart = false;
  }

  /** A message from the /upload/progress progress worker. The server count is the SOLE
   *  upload byte source: `bytes`/`complete` feed the live curve and effective result.
   *  Because there is no client-side fallback, the socket dropping is the
   *  only thing that can leave the up stage without samples — so the worker's
   *  `stall`/`resume` bracket its reconnect. While
   *  the socket is up, the 100 ms frames carry byte/time deltas; on reconnect the
   *  cumulative count + the server's elapsed-time denominator
   *  self-heal the headline. The POST lanes are separate connections, so a progress-
   *  socket drop doesn't stop the transfer: the server keeps draining and accruing
   *  elapsed time, and catch-up Δn / Δelapsed on reconnect is the true rate over
   *  the gap — no client-side counting anywhere. */
  #onMessage(msg: ProgressOutMsg | AuthRequiredMsg): void {
    const lane = this.#deps.lane();
    if (!this.#deps.transferActive() || !lane) return; // late message after teardown
    const host = this.#deps.host();
    if (msg.type === "auth-required") {
      this.#deps.discardTransfer();
      redirectToLogin();
      return;
    }
    if (msg.type === "fatal") {
      this.#ready?.finish(false);
      if (lane.measuring) {
        host.fail(
          "connection-lost",
          `upload progress failed: ${msg.detail}`,
          msg.detail,
        );
      } else {
        host.failStage(lane.stage, "connection-lost", msg.detail);
      }
      return;
    }
    if (msg.type === "stall") {
      // The progress stream dropped: no server bytes until it reconnects. Freeze
      // surface recovery immediately instead of waiting for the silence watchdog.
      if (lane.measuring) this.#deps.setLaneStalled(true, msg.detail);
      return;
    }
    if (msg.type === "resume") {
      // Reopening the control socket is not proof that upload delivery resumed.
      // The next advancing server byte snapshot clears the stall.
      return;
    }
    if (msg.type !== "bytes" && msg.type !== "complete") return; // open: nothing to do

    // `srvT` is elapsed ns since the server received this id's first byte. It is
    // independent of local frame-arrival jitter, while deliberately retaining
    // measured stalls, reconnects and lane turnaround in the denominator.
    const srvT = msg.t;
    if (msg.n > this.#srvN) {
      this.#srvN = msg.n; // cumulative + monotonic guard
      lane.stageSawBytes = true;
    }
    if (!lane.measuring) return; // warmup bytes are excluded from the window

    if (!this.#srvHaveStart) {
      this.#srvHaveStart = true;
      this.#srvPrevN = this.#srvN;
      this.#srvPrevT = srvT;
    }
    // Server bytes drive the live curve directly from the server stream — never
    // via the local #aggregate tick (whose fixed cadence would skew the rate).
    // Each sample is the byte delta between two server frames divided by the
    // server elapsed time between those frames, so the rate is correct at any
    // push cadence and a reconnect catch-up includes the entire gap.
    const delta = this.#srvN - this.#srvPrevN;
    const frameSec = (srvT - this.#srvPrevT) / 1e9;
    this.#srvPrevN = this.#srvN;
    this.#srvPrevT = srvT;
    if (frameSec > 0) {
      host.ingestThroughput("up", delta / frameSec, delta, frameSec, true);
    }
    if (delta > 0) {
      this.#deps.setLaneStalled(false);
    }
    if (msg.type === "complete") this.#done?.();
  }
}
