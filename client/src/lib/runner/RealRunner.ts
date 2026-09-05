// Each stage owns its transport resources; prepared path evidence stays fixed for the run.
import type {
  RunnerConfig,
  PreparedPaths,
  EngineInfo,
  TransportRole,
  FlowDirection,
  PhaseActivity,
  TransferStreamPolicy,
  RecoveryCause,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import { readJSONResponse, parseResponseToken } from "../api/decode";
import { BUILD } from "../buildenv";
import {
  authenticatedFetch,
  authEnabled,
  classifyAuthenticationFailure,
  csrfHeader,
} from "../auth";
import { transferStreamCount } from "./real/streamPolicy";
import {
  needsPings,
  laneUrl,
  sessionDownloadUrl,
  PER_STREAM_BYTES,
  type LaneUrlSpec,
} from "./real/backendPure";
import { transportRunnable } from "./real/transports";
import {
  fetchLane,
  sessionLane,
  type SessionLaneOptions,
} from "./real/byteLane";
import { TransferDirection, transferStageStalled } from "./real/direction";
import { ESTABLISH_BUDGET_MS } from "./real/budgets";
import { LatencyChannel } from "./real/latencyChannel";
import { UploadProgressChannel } from "./real/uploadProgress";
import { UploadPresentationBridge } from "./uploadPresentationBridge";

export class RealBackend implements RunnerBackend {
  #host!: CoreHost;
  readonly #paths: PreparedPaths;
  #stage: TransportStage | null = null;
  #streamPolicy: TransferStreamPolicy = { mode: "auto", count: 1 };
  #cbSeed = "";
  // An upload-id refusal grants one rotation for the whole run, including later stages.
  #uploadRotationUsed = false;

  constructor(paths: PreparedPaths) {
    this.#paths = paths;
  }
  attach(host: CoreHost): void {
    this.#host = host;
  }

  static describe(): EngineInfo {
    return {
      name: "real",
      version: BUILD.clientVersion,
      latencyTransports: transportRunnable("webtransport")
        ? ["webtransport", "websocket"]
        : ["websocket"],
      throughputTransports: transportRunnable("webtransport")
        ? ["fetch-stream", "webtransport", "webtransport-datagram"]
        : ["fetch-stream"],
    };
  }

  onRunStart(config: RunnerConfig): void {
    this.onAbort();
    this.#streamPolicy = { ...config.transferStreams };
    this.#uploadRotationUsed = false;
    this.#cbSeed = `r${Math.round(performance.now())}`;
  }
  onStageBegin(activity: PhaseActivity): void | Promise<void> {
    this.#stage?.discard();
    const stage = new TransportStage(
      this.#host,
      this.#paths,
      activity,
      this.#streamPolicy,
      this.#cbSeed,
    );
    this.#stage = stage;
    return stage.prepare();
  }
  onStageMeasure(_activity: PhaseActivity): void {
    this.#stage?.measure();
  }
  onStageEnd(_activity: PhaseActivity, flush = true): void | Promise<void> {
    const stage = this.#stage;
    if (!stage) return;
    if (!flush) {
      this.#stage = null;
      stage.discard(true);
      return;
    }
    return stage.finish().finally(() => {
      if (this.#stage === stage) this.#stage = null;
    });
  }
  onStageRecovery(request: {
    stage: TransportRole;
    direction?: FlowDirection;
    cause: RecoveryCause;
    signal: AbortSignal;
  }): Promise<void> | void {
    const stage = this.#stage;
    if (
      request.cause !== "unknown-upload-id" ||
      request.direction !== "up" ||
      this.#uploadRotationUsed ||
      stage?.activity.stage !== request.stage ||
      !stage.hasUpload
    )
      return;
    this.#uploadRotationUsed = true;
    return stage.replaceUpload(request.signal);
  }
  onAbort(): void {
    this.#stage?.discard();
    this.#stage = null;
  }
  onComplete(): void {
    this.onAbort();
  }
  dispose(): void {
    this.onAbort();
  }
}

/** A disposable stage captures its paths and callbacks; no continuation can find a later stage's resources. */
class TransportStage {
  readonly #host: CoreHost;
  readonly #paths: PreparedPaths;
  readonly activity: PhaseActivity;
  readonly #policy: TransferStreamPolicy;
  readonly #cbSeed: string;
  readonly #abort = new AbortController();
  #directions: Partial<Record<FlowDirection, TransferDirection>> = {};
  #upload: UploadProgressChannel | null = null;
  #latency: LatencyChannel | null = null;
  #stalled = false;
  #finishing = false;
  #uploadPresentation = new UploadPresentationBridge();
  #presentationTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    host: CoreHost,
    paths: PreparedPaths,
    activity: PhaseActivity,
    policy: TransferStreamPolicy,
    cbSeed: string,
  ) {
    this.#host = host;
    this.#paths = paths;
    this.activity = activity;
    this.#policy = policy;
    this.#cbSeed = cbSeed;
  }

  get hasUpload(): boolean {
    return (
      !this.#finishing && !this.#abort.signal.aborted && this.#upload !== null
    );
  }

  prepare(): void | Promise<void> {
    const target = this.#paths.latency?.target;
    if (needsPings(this.activity)) {
      if (target) {
        this.#latency = new LatencyChannel({
          host: this.#host,
          target,
          stall: (detail) => {
            if (!this.activity.transfer.length) this.#setStalled(true, detail);
          },
          resume: () => {
            if (!this.activity.transfer.length) this.#setStalled(false);
          },
        });
        this.#latency.prime(this.activity.stage === "latency");
      } else if (this.activity.stage === "latency") {
        this.#host.failStage(
          "latency",
          "transport-unavailable",
          "server offers no supported ping transport",
        );
        return;
      }
    }
    if (this.activity.transfer.includes("down")) this.#primeDirection("down");
    if (this.activity.transfer.includes("up")) return this.#prepareUpload();
  }

  measure(): void {
    for (const direction of Object.values(this.#directions))
      direction.measure();
    this.#latency?.measure();
  }

  async finish(): Promise<void> {
    this.#finishing = true;
    this.#clearPresentation();
    try {
      const latency = this.#latency?.finish();
      // Keep each direction measuring while graceful stop delivers its final receiver counters.
      await Promise.all(
        Object.values(this.#directions).map((direction) => direction.stop()),
      );
      if (!this.#abort.signal.aborted) await this.#upload?.finish();
      await latency;
    } finally {
      this.discard();
    }
  }

  discard(incompleteLatency = false): void {
    if (this.#abort.signal.aborted) return;
    this.#abort.abort();
    this.#clearPresentation();
    for (const direction of Object.values(this.#directions))
      direction.discard();
    this.#upload?.discard();
    if (incompleteLatency) this.#latency?.discard();
    else this.#latency?.teardown();
  }

  async replaceUpload(signal: AbortSignal): Promise<void> {
    this.#clearPresentation();
    // Old lane callbacks capture the old meter, which is closed before any replacement is created.
    this.#upload?.discard();
    this.#directions.up?.discard();
    this.#upload = null;
    delete this.#directions.up;
    await this.#prepareUpload(performance.now(), signal);
    if (signal.aborted || this.#abort.signal.aborted) return;
    const replacement = this.#directions.up as TransferDirection | undefined;
    replacement?.setStalled(true, "awaiting replacement upload progress");
    replacement?.measure();
  }

  async #prepareUpload(
    recoveryStartedAt?: number,
    ownerSignal = this.#abort.signal,
  ): Promise<void> {
    let id: string;
    try {
      id = await this.#mintUploadSession(ownerSignal);
    } catch {
      if (!ownerSignal.aborted && !this.#abort.signal.aborted)
        this.#host.failStage(
          this.activity.stage,
          "protocol-error",
          "upload session could not be established",
          "up",
        );
      return;
    }
    if (ownerSignal.aborted || this.#abort.signal.aborted) return;
    await this.#primeDirection("up", id, recoveryStartedAt);
  }

  #primeDirection(
    dir: FlowDirection,
    uploadId?: string,
    recoveryStartedAt?: number,
  ): void | Promise<void> {
    const path = this.#paths.throughput;
    const fetchTarget = path.fetch;
    const wt = path.target.transport === "fetch-stream" ? null : path.target;
    const streams = transferStreamCount({
      protocol: fetchTarget.protocol,
      policy: this.#policy,
      transfer: this.activity.transfer,
      dir,
      needsPing: needsPings(this.activity),
      webTransport: wt !== null,
    });
    const laneCount = wt ? 1 : streams;
    const spec: LaneUrlSpec = {
      dir,
      base: fetchTarget.origin,
      downloadPath: fetchTarget.routes.download,
      uploadPath: fetchTarget.routes.upload,
      cbSeed: this.#cbSeed,
      bytes: PER_STREAM_BYTES,
      session: wt && {
        origin: wt.origin,
        uploadPath: wt.routes.wtUpload,
        downloadPath: wt.routes.wtDownload,
        datagrams: wt.transport === "webtransport-datagram",
      },
    };
    const url = (i: number) => laneUrl(spec, i, uploadId);
    const headers = dir === "up" ? csrfHeader() : {};
    const credentials: RequestCredentials = authEnabled
      ? "include"
      : "same-origin";
    const progressUrl =
      dir === "up" && wt
        ? `${wt.origin}${wt.routes.uploadProgress}?id=${encodeURIComponent(uploadId!)}`
        : undefined;
    const session: SessionLaneOptions | null = wt
      ? {
          url:
            dir === "down"
              ? sessionDownloadUrl(spec.session!, PER_STREAM_BYTES, streams)
              : url(0),
          dir,
          lanes: streams,
          datagrams: wt.transport === "webtransport-datagram",
          mint: authEnabled
            ? {
                url: `${wt.origin}${wt.routes.wtSession}`,
                headers: csrfHeader(),
                credentials: "include",
              }
            : undefined,
          progressUrl,
          headers,
          credentials,
        }
      : null;
    let meter: UploadProgressChannel | undefined;
    const direction = new TransferDirection({
      dir,
      stage: this.activity.stage,
      laneCount,
      warmupMs: this.#host.config!.duration.warmupMs,
      lane: (i, events) =>
        session
          ? sessionLane(session, events)
          : fetchLane(
              { dir, url: url(i), lanes: laneCount, headers, credentials },
              events,
            ),
      host: {
        host: this.#host,
        sampleProvesStageLiveness: () => !this.#stalled,
        stallChanged: (detail, cause, failedDirection) =>
          this.#setStalled(
            transferStageStalled(Object.values(this.#directions)),
            detail,
            cause,
            failedDirection,
          ),
        uploadProgress: (message) => meter?.accept(message),
        beginUploadMeasure: () => meter?.beginMeasure(),
        discardTransfer: () => this.discard(),
        uploadPresentationHint: (lane, bytes, elapsedMs) => {
          this.#uploadPresentation.hint(
            lane,
            bytes,
            elapsedMs,
            performance.now(),
          );
          this.#emitPresentation();
        },
      },
    });
    this.#directions[dir] = direction;
    if (dir === "down") {
      direction.spawn();
      return;
    }
    meter = new UploadProgressChannel({
      host: this.#host,
      target: fetchTarget,
      lane: direction,
      sampleProvesStageLiveness: () => !this.#stalled,
      discardTransfer: () => this.discard(),
      recoveryStartedAt,
      authoritativePresentation: (bytesPerSec) => {
        this.#uploadPresentation.authoritative(
          bytesPerSec,
          true,
          performance.now(),
        );
        this.#emitPresentation();
      },
    });
    this.#upload = meter;
    if (progressUrl) {
      const ready = meter.attachExternal(() => {
        void authenticatedFetch(progressUrl, {
          method: "DELETE",
          cache: "no-store",
          keepalive: true,
          headers,
          credentials,
        }).catch(() => {});
      });
      // WT opens its same-session meter before writing upload streams.
      direction.spawn();
      return ready.then(() => {});
    }
    return meter.prime(uploadId!).then((ready) => {
      if (ready && !this.#abort.signal.aborted) direction.spawn();
    });
  }

  async #mintUploadSession(ownerSignal: AbortSignal): Promise<string> {
    const target = this.#paths.throughput.fetch;
    const timeout = new AbortController();
    const signal = AbortSignal.any([
      ownerSignal,
      this.#abort.signal,
      timeout.signal,
    ]);
    const deadline = setTimeout(() => timeout.abort(), ESTABLISH_BUDGET_MS);
    try {
      const response = await authenticatedFetch(
        `${target.origin}${target.routes.uploadSession}`,
        { method: "POST", cache: "no-store", signal },
      );
      if (!response.ok)
        throw new Error(`upload session returned HTTP ${response.status}`);
      const id = parseResponseToken(
        await readJSONResponse(response),
        "uploadId",
      );
      signal.throwIfAborted();
      return id;
    } catch (cause) {
      await classifyAuthenticationFailure(signal);
      throw cause;
    } finally {
      clearTimeout(deadline);
    }
  }

  #setStalled(
    stalled: boolean,
    detail?: string,
    recoveryCause?: RecoveryCause,
    direction?: FlowDirection,
  ): void {
    if (this.#abort.signal.aborted || stalled === this.#stalled) return;
    this.#stalled = stalled;
    if (stalled) {
      this.#clearPresentation();
      this.#host.stall({
        reason: "connection-lost",
        transport: this.activity.transfer.length
          ? this.#paths.throughput.target.transport
          : this.#paths.latency?.target.transport,
        detail,
        recoveryCause,
        direction,
      });
    } else this.#host.resume();
  }

  #emitPresentation(): void {
    if (this.#abort.signal.aborted) return;
    const healthy =
      !this.#finishing &&
      !this.#stalled &&
      this.#directions.up?.measuring === true;
    const now = performance.now();
    const expectedLanes = this.#directions.up?.laneCount ?? 0;
    this.#host.emit({
      type: "uploadPresentation",
      bytesPerSec: this.#uploadPresentation.target(now, healthy, expectedLanes),
    });
    clearTimeout(this.#presentationTimer);
    const wakeMs = this.#uploadPresentation.nextWakeMs(
      now,
      healthy,
      expectedLanes,
    );
    this.#presentationTimer =
      wakeMs === null
        ? undefined
        : setTimeout(() => this.#emitPresentation(), wakeMs);
  }
  #clearPresentation(): void {
    clearTimeout(this.#presentationTimer);
    this.#uploadPresentation.stop();
    this.#host.emit({ type: "uploadPresentation", bytesPerSec: null });
  }
}
