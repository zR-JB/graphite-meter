// One upload id owns one receiver meter. Replacing the id creates a new instance.
import type { CoreHost } from "../core";
import type { PhaseActivity, RecoveryCause } from "../contract";
import type { FetchThroughputTarget } from "../../api/endpoints";
import { authEnabled, csrfHeader, redirectToLogin } from "../../auth";
import type { AuthRequiredMsg } from "./workerPool";
import { startUploadFeed } from "./uploadFeed";
import {
  ESTABLISH_BUDGET_MS,
  ESTABLISH_MARGIN_MS,
  PROGRESS_FINAL_GRACE_MS,
} from "./budgets";

/** Server cumulative drained bytes and elapsed nanoseconds are the sole upload measurements. */
type ProgressOutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "fatal"; detail: string; cause: RecoveryCause }
  | { type: "stall"; detail: string }
  | { type: "resume" };

export interface UploadProgressLane {
  stage: PhaseActivity["stage"];
  measuring: boolean;
  noteMeasuredProgress(bytes: number): void;
  setStalled(stalled: boolean, detail?: string, cause?: RecoveryCause): void;
}
interface UploadProgressDeps {
  host: CoreHost;
  target: FetchThroughputTarget;
  lane: UploadProgressLane;
  sampleProvesStageLiveness: () => boolean;
  discardTransfer: () => void;
  authoritativePresentation: (bytesPerSec: number) => void;
  recoveryStartedAt?: number;
}

export class UploadProgressChannel {
  #deps: UploadProgressDeps;
  #feed: ReturnType<typeof startUploadFeed> | null = null;
  #ready: ((ready: boolean) => void) | null = null;
  #finalize: (() => void) | null = null;
  #finishing: {
    promise: Promise<void>;
    resolve: () => void;
    timer: ReturnType<typeof setTimeout>;
  } | null = null;
  #closed = false;
  #serverBytes = 0;
  #curveNs: number | null = null;
  #completed = false;
  #recoveryGapStartedAt: number | null;

  constructor(deps: UploadProgressDeps) {
    this.#deps = deps;
    this.#recoveryGapStartedAt = deps.recoveryStartedAt ?? null;
  }

  /** Establish the feed before any POST lane can write. */
  prime(uploadId: string): Promise<boolean> {
    if (this.#closed) return Promise.resolve(false);
    const target = this.#deps.target;
    const ready = this.#awaitReady();
    this.#feed = startUploadFeed({
      url: `${target.origin}${target.routes.uploadProgress}?id=${encodeURIComponent(uploadId)}`,
      csrf: csrfHeader(),
      credentials: authEnabled ? "include" : "same-origin",
      onEvent: (event) => this.accept(event),
    });
    return ready;
  }

  /** The WT session worker carries this feed and normally performs its finalizing DELETE. */
  attachExternal(finalize: () => void): Promise<boolean> {
    this.#finalize = finalize;
    return this.#awaitReady();
  }

  #awaitReady(): Promise<boolean> {
    if (this.#closed) return Promise.resolve(false);
    return new Promise((resolve) => {
      const timer = setTimeout(
        () => this.#ready?.(false),
        ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS,
      );
      this.#ready = (ready) => {
        clearTimeout(timer);
        this.#ready = null;
        resolve(ready);
      };
    });
  }

  beginMeasure(): void {
    this.#curveNs = null;
  }

  /** Lane stop has already delivered WT terminal records; fetch feeds may need a bounded final grace. */
  finish(): Promise<void> {
    if (this.#finishing) return this.#finishing.promise;
    if (this.#closed) return Promise.resolve();
    this.#ready?.(false);
    if (this.#finalize && !this.#completed) this.#finalize();
    if (!this.#feed || this.#completed) {
      this.#close();
      return Promise.resolve();
    }
    let resolve!: () => void;
    const promise = new Promise<void>((done) => {
      resolve = done;
    });
    this.#finishing = {
      promise,
      resolve,
      timer: setTimeout(() => this.#close(), PROGRESS_FINAL_GRACE_MS),
    };
    this.#feed.finalize();
    return promise;
  }

  discard(): void {
    this.#close();
  }

  #close(): void {
    this.#closed = true;
    this.#ready?.(false);
    this.#finalize = null;
    this.#feed?.dispose();
    this.#feed = null;
    if (this.#finishing) {
      clearTimeout(this.#finishing.timer);
      this.#finishing.resolve();
      this.#finishing = null;
    }
  }

  accept(msg: ProgressOutMsg | AuthRequiredMsg): void {
    if (this.#closed) return;
    const { host, lane } = this.#deps;
    if (msg.type === "open") {
      this.#ready?.(true);
      return;
    }
    if (msg.type === "auth-required") {
      this.#deps.discardTransfer();
      redirectToLogin();
      return;
    }
    if (msg.type === "fatal") {
      this.#ready?.(false);
      if (
        !lane.measuring ||
        msg.cause === "capacity-refusal" ||
        msg.cause === "owner-mismatch" ||
        msg.cause === "protocol-refusal"
      )
        host.failStage(lane.stage, "protocol-error", msg.detail, "up");
      else lane.setStalled(true, msg.detail, msg.cause);
      return;
    }
    if (msg.type === "stall") {
      if (lane.measuring) lane.setStalled(true, msg.detail);
      return;
    }
    // Reopening the feed proves no delivery; only an advancing receiver count clears a stall.
    if (msg.type !== "bytes" && msg.type !== "complete") return;
    // Elapsed ns since the server's first byte for this id.
    const serverNs = msg.t;
    const previousServerBytes = this.#serverBytes;
    if (msg.n < previousServerBytes) return; // stale feed: not time evidence
    const advancing = msg.n > previousServerBytes;
    if (advancing) this.#serverBytes = msg.n; // cumulative + monotonic guard
    if (!lane.measuring) return; // warmup bytes are excluded from the window

    // The first measured frame is a baseline; both subsequent deltas use the receiver's counters.
    const delta =
      this.#curveNs === null ? 0 : this.#serverBytes - previousServerBytes;
    const frameSec =
      this.#curveNs === null ? 0 : (serverNs - this.#curveNs) / 1e9;
    this.#curveNs = serverNs;
    if (frameSec > 0) {
      host.ingestThroughput(
        "up",
        delta,
        frameSec,
        true,
        this.#deps.sampleProvesStageLiveness(),
      );
    }
    // The first advancing replacement checkpoint proves delivery despite being only a curve baseline.
    const recoveryGapStartedAt = this.#recoveryGapStartedAt;
    const recovered = advancing && recoveryGapStartedAt !== null;
    if (advancing && recoveryGapStartedAt !== null) {
      const gapSec = (performance.now() - recoveryGapStartedAt) / 1_000;
      this.#recoveryGapStartedAt = null;
      host.recordRecoveryGap("up", gapSec);
      const bytes = this.#serverBytes - previousServerBytes;
      // Replacement bytes are authoritative, but the new id has no preceding server-time interval for a rate.
      host.recordRecoveryBytes("up", bytes);
      lane.noteMeasuredProgress(bytes);
    }
    if (delta > 0) {
      this.#deps.authoritativePresentation(host.presentationRate("up"));
      if (!recovered) lane.noteMeasuredProgress(delta);
    }
    if (msg.type === "complete") {
      this.#completed = true;
      if (this.#finishing) this.#close();
    }
  }
}
