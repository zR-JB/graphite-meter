// The server-authoritative upload meter: the /upload/progress control channel and the server byte/time counters.
import type { CoreHost } from "../core";
import type { PhaseActivity, RecoveryCause } from "../contract";
import type { FetchThroughputTarget } from "../../api/endpoints";
import { authEnabled, csrfHeader, redirectToLogin } from "../../auth";
import { uploadProgressWorker, type AuthRequiredMsg } from "./workerPool";
import {
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  PROGRESS_FINAL_GRACE_MS,
} from "./budgets";

/* `bytes`/`complete` carry the server's cumulative drained count `n` and its elapsed clock `t` (ns), the sole. */
type ProgressOutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string; cause: RecoveryCause }
  | { type: "stall"; detail: string }
  | { type: "resume" };

const PROGRESS_ESTABLISH_TIMEOUT_MS = ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS;

/** The upload lane fields this channel reads and marks. */
export interface UploadProgressLane {
  stage: PhaseActivity["stage"];
  measuring: boolean;
}

interface UploadProgressDeps {
  host: () => CoreHost;
  /** False while another required direction, or this upload, is stalled. */
  sampleProvesStageLiveness?: () => boolean;
  target: () => FetchThroughputTarget | null;
  /** The "up" lane, or undefined once the stage is torn down. */
  lane: () => UploadProgressLane | undefined;
  transferActive: () => boolean;
  discardTransfer: () => void;
  setLaneStalled: (
    stalled: boolean,
    detail?: string,
    cause?: RecoveryCause,
  ) => void;
  /** Positive receiver-authoritative bytes for the upload direction. */
  noteLaneProgress?: (bytes: number) => void;
  /** Current authoritative presentation output after an advancing checkpoint. */
  authoritativePresentation?: (bytesPerSec: number) => void;
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
  /** Rotation handoff awaiting its first advancing replacement counter. */
  #recoveryGapStartedAt: number | null = null;

  get generation(): number {
    return this.#generation;
  }

  #nextGeneration(): number {
    return ++this.#generation;
  }

  /** Reject callbacks from the current feed before its owner detaches it. */
  invalidateGeneration(): void {
    this.#nextGeneration();
  }

  /* Start one reducer-only handoff interval. */
  beginRecoveryGap(): void {
    if (this.#recoveryGapStartedAt === null)
      this.#recoveryGapStartedAt = performance.now();
  }

  constructor(deps: UploadProgressDeps) {
    this.#deps = deps;
  }

  /* Establish the server-authoritative upload progress stream ahead of the POST lanes. */
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
      /* A hard worker error means no server bytes until it recovers, which the stall watchdog covers. */
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

  /* Await a feed an external owner carries: the WebTransport session worker runs it on the same connection as its. */
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

  /* Used by the WebTransport upload worker, whose messages reach the main thread through the lane channel. */
  accept(
    msg: ProgressOutMsg | AuthRequiredMsg,
    generation = this.#generation,
  ): void {
    if (generation !== this.#generation) return;
    if (msg.type === "open") this.#external?.finish("open");
    else if (msg.type === "fatal") this.#external?.finish("superseded");
    this.#onMessage(msg);
  }

  /* Open the measured window: the first progress frame after this boundary becomes the upload baseline, excluding. */
  beginMeasure(): void {
    this.#haveBaseline = false;
    this.#curveBytes = this.#serverBytes;
  }

  /* Stop the feed once the POST lanes finish. */
  teardown(finalize: boolean): Promise<void> {
    this.#recoveryGapStartedAt = null;
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

  /* A session feed finalizes from its own worker, which cannot when the session died before the stage ended. */
  #teardownExternalFeed(finalizeFeed: () => void, finalize: boolean): void {
    if (finalize && !this.#completed) finalizeFeed();
  }

  /* Stop the progress worker. */
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

  /* Drop the worker this channel still owns before it takes another feed. */
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

  /* The worker brackets its reconnect with `stall`/`resume`. */
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
        if (
          msg.cause === "capacity-refusal" ||
          msg.cause === "owner-mismatch" ||
          msg.cause === "protocol-refusal"
        ) {
          host.failStage(lane.stage, "protocol-error", msg.detail, "up");
        } else {
          // The core owns expiry.
          this.#deps.setLaneStalled(true, msg.detail, msg.cause);
        }
      } else {
        host.failStage(lane.stage, "protocol-error", msg.detail, "up");
      }
      return;
    }
    if (msg.type === "stall") {
      // No server bytes arrive until the stream reconnects.
      if (lane.measuring) this.#deps.setLaneStalled(true, msg.detail);
      return;
    }
    if (msg.type === "resume") {
      // Only an advancing server byte snapshot proves upload delivery and clears the stall.
      return;
    }
    if (msg.type !== "bytes" && msg.type !== "complete") return; // open: nothing to do

    // Elapsed ns since the server's first byte for this id.
    const serverNs = msg.t;
    const previousServerBytes = this.#serverBytes;
    if (msg.n < previousServerBytes) return; // stale feed: not time evidence
    const advancing = msg.n > previousServerBytes;
    if (advancing) this.#serverBytes = msg.n; // cumulative + monotonic guard
    if (!lane.measuring) return; // warmup bytes are excluded from the window

    if (!this.#haveBaseline) {
      this.#haveBaseline = true;
      this.#curveBytes = this.#serverBytes;
      this.#curveNs = serverNs;
    }
    // Both the byte delta and elapsed interval come from the server’s receiver clock.
    const delta = this.#serverBytes - this.#curveBytes;
    const frameSec = (serverNs - this.#curveNs) / 1e9;
    this.#curveBytes = this.#serverBytes;
    this.#curveNs = serverNs;
    if (frameSec > 0) {
      host.ingestThroughput(
        "up",
        delta,
        frameSec,
        true,
        this.#deps.sampleProvesStageLiveness?.() ?? true,
      );
    }
    // The first replacement checkpoint is a baseline for the curve, but its advancing server count still proves the.
    const recoveryGapStartedAt = this.#recoveryGapStartedAt;
    const recovered = advancing && recoveryGapStartedAt !== null;
    if (advancing && recoveryGapStartedAt !== null) {
      const gapSec = (performance.now() - recoveryGapStartedAt) / 1_000;
      this.#recoveryGapStartedAt = null;
      host.recordRecoveryGap("up", gapSec);
      const bytes = this.#serverBytes - previousServerBytes;
      // This count is authoritative final-reducer evidence, but cannot be a rate sample: a replacement id has no.
      host.recordRecoveryBytes("up", bytes);
      if (this.#deps.noteLaneProgress) this.#deps.noteLaneProgress(bytes);
      else this.#deps.setLaneStalled(false);
    }
    if (delta > 0) {
      if (this.#deps.authoritativePresentation)
        this.#deps.authoritativePresentation(host.presentationRate("up"));
      if (!recovered && this.#deps.noteLaneProgress)
        this.#deps.noteLaneProgress(delta);
      else if (!recovered) this.#deps.setLaneStalled(false);
    }
    if (msg.type === "complete") {
      this.#completed = true;
      this.#done?.();
    }
  }
}
