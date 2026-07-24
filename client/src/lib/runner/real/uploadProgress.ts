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
  /** Latest cumulative byte count the server reported. */
  #serverBytes = 0;
  /** Cumulative count at the last delta fed into the live curve. */
  #curveBytes = 0;
  /** Server elapsed ns of that last delta — the live-curve denominator. */
  #curveNs = 0;
  /** True once the measured window has its baseline frame. */
  #haveBaseline = false;

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
    const worker = uploadProgressWorker();
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
    worker.onmessage = (e: MessageEvent<ProgressOutMsg>): void => {
      if (e.data.type === "open") this.#ready?.finish(true);
      this.#onMessage(e.data);
    };
    worker.onerror = (): void => {
      /* the worker owns reconnect; a hard worker error just means no server bytes
       * until it recovers, which the stall watchdog already covers. */
    };
    worker.postMessage({
      type: "start",
      url,
      headers: this.#deps.headers(),
      csrf: csrfHeader(),
      credentials: authEnabled ? "include" : "same-origin",
    });
    this.#worker = worker;
    return ready;
  }

  /** Open the measured window: the first progress frame after this boundary
   *  becomes the upload baseline, excluding warmup bytes and time together. */
  beginMeasure(): void {
    this.#haveBaseline = false;
    this.#curveBytes = this.#serverBytes;
  }

  /** Stop the progress worker after the POST lanes. It explicitly finalizes the
   *  session with DELETE and lets the stream receive the terminal complete record. */
  teardown(finalize: boolean): Promise<void> {
    this.#ready?.finish(false);
    this.#ready = null;
    const worker = this.#worker;
    if (!worker) return Promise.resolve();
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
      const done = (): void => {
        clearTimeout(timer);
        if (this.#done === done) this.#done = null;
        if (this.#worker === worker) this.#worker = null;
        worker.terminate();
        resolve();
      };
      const timer = setTimeout(done, PROGRESS_BYE_GRACE_MS);
      this.#done = done;
      worker.postMessage({ type: "stop" });
    });
  }

  #resetCounters(): void {
    this.#serverBytes = 0;
    this.#curveBytes = 0;
    this.#curveNs = 0;
    this.#haveBaseline = false;
  }

  /** A message from the /upload/progress worker. The server count is the SOLE
   *  upload byte source: `bytes`/`complete` feed both the live curve and the
   *  effective result, so losing this socket is the only thing that can leave the
   *  up stage without samples — hence the worker's `stall`/`resume` around its
   *  reconnect. The POST lanes are separate connections, so a progress-socket
   *  drop does not stop the transfer: the server keeps draining and accruing
   *  elapsed time, and the catch-up Δbytes / Δelapsed on reconnect is the true
   *  rate over the gap. */
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

    // Elapsed ns since the server received this id's first byte. It is
    // independent of local frame-arrival jitter, while deliberately retaining
    // measured stalls, reconnects and lane turnaround in the denominator.
    const serverNs = msg.t;
    if (msg.n > this.#serverBytes) {
      this.#serverBytes = msg.n; // cumulative + monotonic guard
      lane.stageSawBytes = true;
    }
    if (!lane.measuring) return; // warmup bytes are excluded from the window

    if (!this.#haveBaseline) {
      this.#haveBaseline = true;
      this.#curveBytes = this.#serverBytes;
      this.#curveNs = serverNs;
    }
    // Server bytes drive the live curve directly from the server stream — never
    // via the local #aggregate tick (whose fixed cadence would skew the rate).
    // Each sample is the byte delta between two server frames divided by the
    // server elapsed time between those frames, so the rate is correct at any
    // push cadence and a reconnect catch-up includes the entire gap.
    const delta = this.#serverBytes - this.#curveBytes;
    const frameSec = (serverNs - this.#curveNs) / 1e9;
    this.#curveBytes = this.#serverBytes;
    this.#curveNs = serverNs;
    if (frameSec > 0) {
      host.ingestThroughput("up", delta / frameSec, delta, frameSec, true);
    }
    if (delta > 0) {
      this.#deps.setLaneStalled(false);
    }
    if (msg.type === "complete") this.#done?.();
  }
}
