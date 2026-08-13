// The server-authoritative upload meter: the /upload/progress control channel
// and the server byte/time counters derived from it.
import type { CoreHost } from "../core";
import type { PhaseActivity } from "../contract";
import type { FetchThroughputTarget } from "../../api/endpoints";
import { authEnabled, csrfHeader, redirectToLogin } from "../../auth";
import { uploadProgressWorker, type AuthRequiredMsg } from "./workerPool";
import {
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  PROGRESS_FINAL_GRACE_MS,
} from "./budgets";

/** Upload-progress worker → channel messages. `bytes`/`complete` carry the
 *  server's cumulative drained count `n` and its elapsed clock `t` (ns), the
 *  sole upload byte source. Rate derives over server time (Δn / Δt), so curve
 *  and totals are immune to local tick jitter. stall/resume bracket recovery. */
type ProgressOutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string }
  | { type: "stall"; detail: string }
  | { type: "resume" };

const PROGRESS_ESTABLISH_TIMEOUT_MS = ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS;

/** The upload lane fields this channel reads and marks. */
export interface UploadProgressLane {
  stage: PhaseActivity["stage"];
  measuring: boolean;
  stageSawBytes: boolean;
}

export interface UploadProgressDeps {
  host: () => CoreHost;
  /** False while another required direction, or this upload, is stalled. */
  sampleProvesStageLiveness?: () => boolean;
  target: () => FetchThroughputTarget | null;
  /** The "up" lane, or undefined once the stage is torn down. */
  lane: () => UploadProgressLane | undefined;
  transferActive: () => boolean;
  discardTransfer: () => void;
  setLaneStalled: (stalled: boolean, detail?: string) => void;
  /** Positive receiver-authoritative bytes for the upload direction. */
  noteLaneProgress?: (bytes: number) => void;
}

export class UploadProgressChannel {
  #deps: UploadProgressDeps;
  #worker: Worker | null = null;
  #ready: { finish: (ready: boolean) => void } | null = null;
  /** Pending external attach (WebTransport), resolved open/timeout/superseded. */
  #external: {
    finish: (state: "open" | "timeout" | "superseded") => void;
  } | null = null;
  /** Sends the finalizing DELETE for an external feed whose owner did not. */
  #finalize: (() => void) | null = null;
  #done: (() => void) | null = null;
  /** Latest cumulative byte count the server reports. */
  #serverBytes = 0;
  /** Cumulative count at the last delta fed into the live curve. */
  #curveBytes = 0;
  /** Server elapsed ns of that last delta, the live-curve denominator. */
  #curveNs = 0;
  /** True once the measured window has its baseline frame. */
  #haveBaseline = false;
  /** True once the terminal complete record landed for this stage. */
  #completed = false;
  /** Local ownership token for a server upload id/feed pair. */
  #generation = 0;

  get generation(): number {
    return this.#generation;
  }

  #nextGeneration(): number {
    return ++this.#generation;
  }

  constructor(deps: UploadProgressDeps) {
    this.#deps = deps;
  }

  /** Establish the server-authoritative upload progress stream ahead of the
   *  POST lanes. Upload cannot be measured honestly without this channel. */
  prime(stage: PhaseActivity["stage"], uploadId: string): Promise<boolean> {
    this.#releaseWorker();
    this.#resetCounters();
    const generation = this.#nextGeneration();

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
      if (generation !== this.#generation) return;
      if (e.data.type === "open") this.#ready?.finish(true);
      this.#onMessage(e.data);
    };
    worker.onerror = (): void => {
      /* the worker owns reconnect. A hard worker error means no server bytes
       * until it recovers, which the stall watchdog covers. */
    };
    worker.postMessage({
      type: "start",
      url,
      csrf: csrfHeader(),
      credentials: authEnabled ? "include" : "same-origin",
    });
    this.#worker = worker;
    return ready;
  }

  /** Await a feed an external owner carries: the WebTransport session worker
   *  runs it on the same connection as its lanes and owns the finalizing
   *  DELETE. "superseded" means the lane was torn down or replaced first, which
   *  is not a stage failure: exactly one owner may act on the outcome. */
  attachExternal(
    finalize: () => void,
  ): Promise<"open" | "timeout" | "superseded"> {
    this.#external?.finish("superseded");
    this.#resetCounters();
    this.#releaseWorker();
    this.#nextGeneration();
    this.#finalize = finalize;
    return new Promise((resolve) => {
      const finish = (state: "open" | "timeout" | "superseded"): void => {
        if (this.#external?.finish !== finish) return;
        clearTimeout(timer);
        this.#external = null;
        resolve(state);
      };
      const timer = setTimeout(
        () => finish("timeout"),
        PROGRESS_ESTABLISH_TIMEOUT_MS,
      );
      this.#external = { finish };
    });
  }

  /** Feed one relayed record in. Used by the WebTransport upload worker, whose
   *  messages reach the main thread through the lane channel. A refusal ends
   *  the attach as surely as a ready record: leaving it pending would fail the
   *  stage once for the refusal and again when the wait times out. */
  accept(
    msg: ProgressOutMsg | AuthRequiredMsg,
    generation = this.#generation,
  ): void {
    if (generation !== this.#generation) return;
    if (msg.type === "open") this.#external?.finish("open");
    else if (msg.type === "fatal") this.#external?.finish("superseded");
    this.#onMessage(msg);
  }

  /** Open the measured window: the first progress frame after this boundary
   *  becomes the upload baseline, excluding warmup bytes and time together. */
  beginMeasure(): void {
    this.#haveBaseline = false;
    this.#curveBytes = this.#serverBytes;
  }

  /** Stop the feed once the POST lanes finish. `finalize` sends the terminating
   *  DELETE and waits for the terminal complete record; without it the feed is
   *  simply dropped. Exactly one of the two feed kinds is ever live. */
  teardown(finalize: boolean): Promise<void> {
    this.#external?.finish("superseded");
    this.#ready?.finish(false);
    this.#ready = null;
    const external = this.#finalize;
    this.#finalize = null;
    if (external) {
      this.#teardownExternalFeed(external, finalize);
      return Promise.resolve();
    }
    return this.#teardownWorkerFeed(finalize);
  }

  /** A session feed finalizes from its own worker, which cannot when the
   *  session died before the stage ended. Without a terminal record there is
   *  nothing to wait for, and the DELETE is idempotent. */
  #teardownExternalFeed(finalizeFeed: () => void, finalize: boolean): void {
    if (finalize && !this.#completed) finalizeFeed();
  }

  /** Stop the progress worker. It gets the BYE grace to deliver the terminal
   *  record, unless one already landed or the caller is discarding the stage. */
  #teardownWorkerFeed(finalize: boolean): Promise<void> {
    const worker = this.#worker;
    if (!worker) return Promise.resolve();
    // The terminal record already landed: nothing to wait on.
    if (this.#completed) {
      this.#worker = null;
      worker.postMessage({ type: "stop" });
      worker.terminate();
      return Promise.resolve();
    }
    if (!finalize) {
      // A grace already running owns the worker; resolving it terminates.
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
      const timer = setTimeout(done, PROGRESS_FINAL_GRACE_MS);
      this.#done = done;
      worker.postMessage({ type: "stop" });
    });
  }

  /** Drop the worker this channel still owns before it takes another feed.
   *  Every current path tears down first; a worker that did outlive its stage
   *  would keep routing its upload id's cumulative count, which the monotonic
   *  guard accepts, into the next stage's meter. */
  #releaseWorker(): void {
    if (!this.#worker) return;
    this.#worker.terminate();
    this.#worker = null;
  }

  #resetCounters(): void {
    this.#completed = false;
    this.#serverBytes = 0;
    this.#curveBytes = 0;
    this.#curveNs = 0;
    this.#haveBaseline = false;
  }

  /** A message from the /upload/progress worker. Server counts are the sole
   *  upload byte source, so a dropped socket is the only way the up stage ends
   *  without samples. The worker brackets its reconnect with `stall`/`resume`.
   *  POST lanes are separate connections and keep uploading across that gap. */
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
      // No server bytes arrive until the stream reconnects. Mark the lane
      // stalled immediately rather than waiting for the silence watchdog.
      if (lane.measuring) this.#deps.setLaneStalled(true, msg.detail);
      return;
    }
    if (msg.type === "resume") {
      // A reopened control socket is not proof of upload delivery. The next
      // advancing server byte snapshot clears the stall.
      return;
    }
    if (msg.type !== "bytes" && msg.type !== "complete") return; // open: nothing to do

    // Elapsed ns since the server's first byte for this id. Free of local
    // arrival jitter, and it retains stalls, reconnects and lane turnaround.
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
    // Each curve sample is Δbytes over Δserver-elapsed between two frames, so
    // the rate holds at any push cadence and a catch-up covers the whole gap.
    const delta = this.#serverBytes - this.#curveBytes;
    const frameSec = (serverNs - this.#curveNs) / 1e9;
    this.#curveBytes = this.#serverBytes;
    this.#curveNs = serverNs;
    if (frameSec > 0) {
      host.ingestThroughput(
        "up",
        delta / frameSec,
        delta,
        frameSec,
        true,
        this.#deps.sampleProvesStageLiveness?.() ?? true,
      );
    }
    if (delta > 0) {
      if (this.#deps.noteLaneProgress) this.#deps.noteLaneProgress(delta);
      else this.#deps.setLaneStalled(false);
    }
    if (msg.type === "complete") {
      this.#completed = true;
      this.#done?.();
    }
  }
}
