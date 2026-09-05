// Run-owned transport resources use the paths already verified by the application.
import type {
  RunnerConfig,
  PreparedPaths,
  EngineInfo,
  TransportKind,
  TransportRole,
  FlowDirection,
  PhaseActivity,
  TransferStreamPolicy,
  RecoveryCause,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../api/endpoints";
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
import { ridesSession, transportRunnable } from "./real/transports";
import {
  fetchLane,
  sessionLane,
  type SessionLaneOptions,
} from "./real/byteLane";
import {
  TransferDirection,
  transferStageStalled,
  type DirectionHost,
} from "./real/direction";
import { ESTABLISH_BUDGET_MS } from "./real/budgets";
import { LatencyChannel } from "./real/latencyChannel";
import { UploadProgressChannel } from "./real/uploadProgress";
import { UploadPresentationBridge } from "./uploadPresentationBridge";

export class RealBackend implements RunnerBackend {
  /** The core handle: push samples / emit / report failures + health through it. */
  #host: CoreHost | null = null;
  /** AbortController for in-flight fetches/streams; aborted in onAbort. */
  #abort: AbortController | null = null;
  /** The transport established for the active phase, for stall/fail reporting. */
  #activeTransport: TransportKind | null = null;
  readonly #throughputTarget: FetchThroughputTarget;
  readonly #wtThroughputTarget: WebTransportThroughputTarget | null;
  readonly #latencyTarget: LatencyTarget | null;
  #streamPolicy: TransferStreamPolicy = { mode: "auto", count: 1 };

  constructor(paths: PreparedPaths) {
    this.#throughputTarget = paths.throughput.fetch;
    this.#wtThroughputTarget =
      paths.throughput.target.transport === "fetch-stream"
        ? null
        : paths.throughput.target;
    this.#latencyTarget = paths.latency?.target ?? null;
  }

  /* Transfer stage state: bidirectional primes both directions on the same stage. */
  /** One lane pool + its bookkeeping, per active transfer direction. */
  #lanes: Partial<Record<FlowDirection, TransferDirection>> = {};
  /* Active from the stage's first #primeTransfer through #teardownTransfer. */
  #transferActive = false;
  /** The activity whose transfer connections are currently alive. */
  #transferActivity: PhaseActivity | null = null;
  /* Invalidates teardown continuations when a later run or stage owns the shared lane/feed fields. */
  #transferGeneration = 0;
  /* A second upload-id invalidation belongs to runner expiry, not an unbounded mint loop. */
  #uploadRotationUsed = false;
  #uploadRotationInFlight = false;
  /** A local-only visual bridge over irregular authoritative upload delivery. */
  #uploadPresentation = new UploadPresentationBridge();
  #uploadPresentationTimer: ReturnType<typeof setTimeout> | null = null;
  /* The STAGE-level stalled flag reported to the host, deduped so stall/resume fire once per edge. */
  #stalled = false;
  /** Per-run cache-buster seed, so `?cb=` is unique across runs and streams. */
  #cbSeed = "";

  /** The server-authoritative upload meter (up stage only). */
  #uploadProgress = new UploadProgressChannel({
    host: () => this.#host!,
    sampleProvesStageLiveness: () => !this.#stalled,
    target: () => this.#throughputTarget,
    lane: () => this.#lanes.up,
    transferActive: () => this.#transferActive,
    discardTransfer: () => this.#discardTransfer(),
    noteLaneProgress: (bytes) => this.#lanes.up?.noteMeasuredProgress(bytes),
    authoritativePresentation: (bytesPerSec) => {
      this.#uploadPresentation.authoritative(
        bytesPerSec,
        true,
        performance.now(),
      );
      this.#emitUploadPresentation();
    },
    setLaneStalled: (stalled, detail, cause) =>
      this.#lanes.up?.setStalled(stalled, detail, cause),
  });

  /** Provides each transfer direction with stage-level host callbacks. */
  #directionHost: DirectionHost = {
    host: () => this.#host!,
    sampleProvesStageLiveness: () => !this.#stalled,
    stallChanged: (detail, cause, direction) =>
      this.#reconcileStall(detail, cause, direction),
    uploadProgress: (msg, generation) =>
      this.#uploadProgress.accept(msg, generation),
    uploadPresentationHint: (lane, bytes, elapsedMs, generation) => {
      if (generation !== this.#uploadProgress.generation) return;
      this.#uploadPresentation.hint(lane, bytes, elapsedMs, performance.now());
      this.#emitUploadPresentation();
    },
    beginUploadMeasure: () => this.#uploadProgress.beginMeasure(),
    discardTransfer: () => this.#discardTransfer(),
  };

  /* Its stall/resume reach the core ONLY for the idle latency stage; during a transfer stage the byte lanes drive. */
  #latency = new LatencyChannel({
    host: () => this.#host!,
    target: () => this.#latencyTarget,
    stall: (detail) => {
      if (!this.#transferActive && !this.#stalled) {
        this.#host!.stall({
          reason: "connection-lost",
          transport: this.#latencyTarget?.transport ?? "websocket",
          detail,
        });
        this.#stalled = true;
      }
    },
    resume: () => {
      if (!this.#transferActive && this.#stalled) {
        this.#host!.resume();
        this.#stalled = false;
      }
    },
  });

  attach(host: CoreHost): void {
    this.#host = host;
  }

  /* What this engine can drive, per role: WebSocket and WebTransport pings, fetch-stream and WebTransport transfer. */
  static describe(): EngineInfo {
    return {
      name: "real",
      version: BUILD.clientVersion, // built with the client
      latencyTransports: transportRunnable("webtransport")
        ? ["webtransport", "websocket"]
        : ["websocket"],
      // Throughput preference leads with fetch: TCP streams usually win raw rate, so sessions require explicit choice.
      throughputTransports: transportRunnable("webtransport")
        ? ["fetch-stream", "webtransport", "webtransport-datagram"]
        : ["fetch-stream"],
    };
  }

  /* ================= LIFECYCLE (core → backend) ================= */
  onRunStart(config: RunnerConfig): void {
    this.#streamPolicy = { ...config.transferStreams };
    this.#abort = new AbortController();
    this.#activeTransport = null;
    this.#clearUploadPresentation();
    this.#discardTransfer(); // discard leftovers from a prior run
    this.#transferActivity = null;
    this.#uploadRotationUsed = false;
    this.#uploadRotationInFlight = false;
    this.#stalled = false;
    // Unique-per-run cache-buster off the monotonic clock, not wall time; the stream index is appended per worker.
    this.#cbSeed = `r${Math.round(performance.now())}`;
  }

  onStageBegin(activity: PhaseActivity): void | Promise<void> {
    const preparations: Promise<void>[] = [];
    // The ping channel is ALWAYS a latency-role transport on its OWN socket.
    if (needsPings(activity)) {
      const pingKind = this.#latencyTarget?.transport ?? null;
      if (pingKind) {
        this.#latency.prime(pingKind, activity.stage === "latency");
      } else if (activity.stage === "latency") {
        this.#host!.failStage(
          "latency",
          "transport-unavailable",
          "server offers no supported ping transport",
        );
        return;
      }
      // Loaded latency without a transport: run the transfer without pings.
    }
    if (activity.transfer.length > 0) {
      this.#transferGeneration++;
      this.#transferActivity = activity;
      const kind = this.#wtThroughputTarget?.transport ?? "fetch-stream";
      this.#activeTransport = kind;
      for (const dir of activity.transfer) {
        const preparation = this.#primeTransfer(kind, dir, activity);
        if (preparation) preparations.push(preparation);
      }
    }
    if (preparations.length > 0)
      return Promise.all(preparations).then(() => undefined);
  }

  /* `underLoad` marks pings taken while the stage moves bytes (bufferbloat). */
  onStageMeasure(activity: PhaseActivity): void {
    // A direction missing here failed to prime, and has nothing to measure.
    for (const dir of activity.transfer) this.#lanes[dir]?.measure();
    if (needsPings(activity)) this.#latency.measure();
  }

  onStageEnd(_activity: PhaseActivity, flush = true): void | Promise<void> {
    if (!flush) {
      this.#discardTransfer();
      this.#latency.discard();
      // Reset the stage stall latch; otherwise later healthy bytes cannot refresh the core watchdog.
      this.#stalled = false;
      this.#transferActivity = null;
      return;
    }
    const generation = this.#transferGeneration;
    const latencyFinished = this.#latency.finish();
    return Promise.all([
      this.#teardownTransfer(generation),
      latencyFinished,
    ]).then(([released]) => {
      // Ignore completion if abort or a new run took ownership while this stage awaited its terminal record.
      if (!released || generation !== this.#transferGeneration) return;
      this.#stalled = false;
      this.#transferActivity = null;
    });
  }

  /** The run is complete. Close anything still open. */
  onComplete(): void {
    this.#closeAll();
  }

  async onStageRecovery(request: {
    stage: TransportRole;
    direction?: FlowDirection;
    cause: RecoveryCause;
    signal: AbortSignal;
  }): Promise<void> {
    if (
      request.cause !== "unknown-upload-id" ||
      request.direction !== "up" ||
      this.#uploadRotationUsed ||
      this.#uploadRotationInFlight ||
      this.#transferActivity?.stage !== request.stage ||
      !this.#transferActive ||
      !this.#activeTransport
    )
      return;
    this.#uploadRotationUsed = true;
    this.#uploadRotationInFlight = true;
    this.#clearUploadPresentation();
    try {
      // The old feed and every session-lane callback become inert before the old lanes are detached.
      this.#uploadProgress.invalidateGeneration();
      this.#lanes.up?.discard();
      await this.#uploadProgress.teardown(false);
      if (request.signal.aborted || !this.#transferActive) return;
      delete this.#lanes.up;

      const activity = this.#transferActivity;
      const kind = this.#activeTransport;
      if (!activity || !kind) return;
      this.#uploadProgress.beginRecoveryGap();
      await this.#primeTransfer(kind, "up", activity);
      if (request.signal.aborted) return;
      const replacement = this.#lanes.up as TransferDirection | undefined;
      if (!this.#transferActive || !replacement) return;
      // The replacement remains recovering until a positive authoritative counter flips this edge back to healthy.
      replacement.setStalled(true, "awaiting replacement upload progress");
      replacement.measure();
    } finally {
      this.#uploadRotationInFlight = false;
    }
  }

  onAbort(): void {
    this.#closeAll();
  }

  /* Token mint the WebTransport workers call before each dial. */
  #wtMint(origin: string, route: string): SessionLaneOptions["mint"] {
    if (!authEnabled) return undefined;
    return {
      url: `${origin}${route}`,
      headers: csrfHeader(),
      credentials: "include",
    };
  }

  /* ================= PRIME (warmup window): open, don't measure ================= */
  /* Open `dir` streams over `kind`: GET download bytes or streamed POST upload bytes, then prime the path. */
  #primeTransfer(
    kind: TransportKind,
    dir: FlowDirection,
    activity: PhaseActivity,
  ): void | Promise<void> {
    // A session kind is returned only with a resolved session target, so it always finds one here.
    const wt = ridesSession(kind) ? this.#wtThroughputTarget : null;

    // Each stage names a direction once; a duplicate call for the same direction is a bug.
    if (this.#lanes[dir]) throw new Error(`duplicate ${dir} prime`);

    const cfg = this.#host!.config!;
    const base = this.#throughputTarget.origin;
    const streams = transferStreamCount({
      protocol: this.#throughputTarget.protocol,
      policy: this.#streamPolicy,
      transfer: activity.transfer,
      dir,
      needsPing: needsPings(activity),
      webTransport: this.#wtThroughputTarget !== null,
    });
    // A WebTransport session uses one worker; it opens `streams` lanes internally.
    const laneCount = wt ? 1 : streams;

    const direction: TransferDirection = new TransferDirection({
      dir,
      stage: activity.stage,
      laneCount,
      warmupMs: cfg.duration.warmupMs,
      host: this.#directionHost,
      // The session factory replaces this lane when the direction rides a session.
      lane: (i, events) =>
        fetchLane(
          {
            dir,
            url: direction.streamUrls[i],
            lanes: laneCount,
            credentials: authEnabled ? "include" : "same-origin",
            // CSRF is needed on upload POST; adding it to download GET triggers a cross-port CORS preflight.
            headers: dir === "up" ? csrfHeader() : {},
          },
          events,
        ),
    });
    this.#lanes[dir] = direction;
    this.#transferActive = true;

    // Download streams the body down (?bytes=N sizes it); upload streams a generated body up until the stage stops.
    const spec: LaneUrlSpec = {
      dir,
      base,
      downloadPath: this.#throughputTarget.routes.download,
      uploadPath: this.#throughputTarget.routes.upload,
      cbSeed: this.#cbSeed,
      bytes: PER_STREAM_BYTES,
      session: wt && {
        origin: wt.origin,
        uploadPath: wt.routes.wtUpload,
        downloadPath: wt.routes.wtDownload,
        datagrams: wt.transport === "webtransport-datagram",
      },
    };
    const url = (i: number, uploadId?: string): string =>
      laneUrl(spec, i, uploadId);

    if (dir === "up")
      return this.#primeUploadTransfer(dir, base, laneCount, url, streams, wt);

    if (wt) {
      const sessionOpts = {
        url: sessionDownloadUrl(spec.session!, PER_STREAM_BYTES, streams),
        dir,
        lanes: streams,
        datagrams: spec.session!.datagrams,
        mint: this.#wtMint(wt.origin, wt.routes.wtSession),
      };
      direction.newLane = (_i, events) => sessionLane(sessionOpts, events);
      direction.spawn([sessionOpts.url]);
      return;
    }
    direction.spawn(Array.from({ length: laneCount }, (_, i) => url(i)));
    // Warmup download progress carries seq=0 and is ignored, so no warmup bytes bleed into measurement.
  }

  /* Mint the upload session ID, establish its meter, then open POST or session upload lanes. */
  async #primeUploadTransfer(
    dir: FlowDirection,
    base: string,
    laneCount: number,
    url: (i: number, uploadId?: string) => string,
    streams: number,
    wt: WebTransportThroughputTarget | null,
  ): Promise<void> {
    let id: string;
    try {
      id = await this.#mintUploadSession(base);
    } catch {
      return;
    }
    const uploadLane = this.#transferActive ? this.#lanes[dir] : undefined;
    if (!uploadLane) return;
    if (wt) {
      const progressUrl = `${wt.origin}${wt.routes.uploadProgress}?id=${encodeURIComponent(id)}`;
      const headers = csrfHeader();
      const credentials: RequestCredentials = authEnabled
        ? "include"
        : "same-origin";
      const sessionOpts = {
        url: url(0, id),
        dir,
        lanes: streams,
        datagrams: wt.transport === "webtransport-datagram",
        mint: this.#wtMint(wt.origin, wt.routes.wtSession),
        progressUrl,
        headers,
        credentials,
      };
      const feed = this.#uploadProgress.attachExternal(() => {
        void fetch(progressUrl, {
          method: "DELETE",
          cache: "no-store",
          keepalive: true,
          headers,
          credentials,
        }).catch(() => {});
      });
      uploadLane.setUploadGeneration(this.#uploadProgress.generation);
      // The session worker reads the feed before its lanes write, so the counter is already running when bytes start.
      uploadLane.newLane = (_i, events) => sessionLane(sessionOpts, events);
      uploadLane.spawn([sessionOpts.url]);
      await feed;
      return;
    }
    // The progress stream is the authoritative upload meter.
    if (!(await this.#uploadProgress.prime(uploadLane.stage, id))) return;
    const progressLane = this.#transferActive ? this.#lanes[dir] : undefined;
    if (!progressLane) return;
    progressLane.setUploadGeneration(this.#uploadProgress.generation);
    progressLane.spawn(Array.from({ length: laneCount }, (_, i) => url(i, id)));
  }

  async #mintUploadSession(base: string): Promise<string> {
    const path = this.#throughputTarget.routes.uploadSession;
    // Own the deadline and run abort so a hanging fetch rejects and the stage skips instead of max-stalling.
    const ctl = new AbortController();
    const onRunAbort = (): void => ctl.abort();
    this.#abort?.signal.addEventListener("abort", onRunAbort, { once: true });
    const deadline = setTimeout(() => ctl.abort(), ESTABLISH_BUDGET_MS);
    try {
      const res = await authenticatedFetch(`${base}${path}`, {
        method: "POST",
        cache: "no-store",
        signal: ctl.signal,
      });
      if (!res.ok)
        throw new Error(`upload session returned HTTP ${res.status}`);
      return parseResponseToken(await readJSONResponse(res), "uploadId");
    } catch (cause) {
      await classifyAuthenticationFailure(ctl.signal);
      throw cause;
    } finally {
      clearTimeout(deadline);
      this.#abort?.signal.removeEventListener("abort", onRunAbort);
    }
  }

  /* Combine the directions into the STAGE-level flag. */
  #reconcileStall(
    detail?: string,
    recoveryCause?: RecoveryCause,
    direction?: FlowDirection,
  ): void {
    const transferStalled = transferStageStalled(
      Object.values(this.#lanes) as TransferDirection[],
    );
    if (transferStalled && !this.#stalled) {
      this.#clearUploadPresentation();
      this.#host!.stall({
        reason: "connection-lost",
        transport: this.#activeTransport ?? undefined,
        detail,
        recoveryCause,
        direction,
      });
      this.#stalled = true;
    } else if (!transferStalled && this.#stalled) {
      this.#host!.resume();
      this.#stalled = false;
    }
  }

  /** Stop every POST lane, wait for the server's terminal upload count, then release the stage state. */
  async #teardownTransfer(generation: number): Promise<boolean> {
    this.#clearUploadPresentation();
    // A graceful WT stop finalizes in the worker; the progress teardown then has nothing left to wait on.
    await Promise.all(Object.values(this.#lanes).map((d) => d!.stop()));
    if (generation !== this.#transferGeneration) return false;
    await this.#uploadProgress.teardown(true);
    if (generation !== this.#transferGeneration) return false;
    this.#transferActive = false;
    this.#lanes = {};
    this.#transferActivity = null;
    return true;
  }

  #discardTransfer(): void {
    // Any outstanding graceful teardown now belongs to an older lifecycle.
    this.#transferGeneration++;
    this.#clearUploadPresentation();
    for (const direction of Object.values(this.#lanes)) direction!.discard();
    void this.#uploadProgress.teardown(false);
    this.#transferActive = false;
    this.#lanes = {};
    this.#transferActivity = null;
  }

  #closeAll(): void {
    this.#discardTransfer();
    this.#latency.teardown();
    this.#stalled = false;
    if (this.#abort) {
      this.#abort.abort();
      this.#abort = null;
    }
    this.#activeTransport = null;
  }

  /* Emit only a temporary target for the gauge and compact live card. */
  #emitUploadPresentation(): void {
    const healthy =
      this.#transferActive &&
      !this.#stalled &&
      !this.#uploadRotationInFlight &&
      this.#lanes.up?.measuring === true;
    const now = performance.now();
    const expectedLanes = this.#lanes.up?.laneCount ?? 0;
    const bytesPerSec = this.#uploadPresentation.target(
      now,
      healthy,
      expectedLanes,
    );
    this.#host?.emit({ type: "uploadPresentation", bytesPerSec });
    if (this.#uploadPresentationTimer)
      clearTimeout(this.#uploadPresentationTimer);
    const wakeMs = this.#uploadPresentation.nextWakeMs(
      now,
      healthy,
      expectedLanes,
    );
    this.#uploadPresentationTimer =
      wakeMs === null
        ? null
        : setTimeout(() => {
            this.#uploadPresentationTimer = null;
            this.#emitUploadPresentation();
          }, wakeMs);
  }

  #clearUploadPresentation(): void {
    if (this.#uploadPresentationTimer)
      clearTimeout(this.#uploadPresentationTimer);
    this.#uploadPresentationTimer = null;
    this.#uploadPresentation.stop();
    this.#host?.emit({ type: "uploadPresentation", bytesPerSec: null });
  }

  dispose(): void {
    this.#closeAll();
  }
}
