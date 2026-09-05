// Real measurement backend: negotiates browser transports, drives the transfer lanes and the latency/upload-progress.
import type {
  RunnerConfig,
  InfraInfo,
  EngineInfo,
  TransportKind,
  TransportDiscovery,
  TransportRole,
  FlowDirection,
  PhaseActivity,
  TransferStreamPolicy,
  ConnectionRole,
  RecoveryCause,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../api/endpoints";
import type { Preflight } from "../api/preflight";
import {
  readJSONResponse,
  parsePreflight,
  parseProbe,
  parseResponseToken,
} from "../api/decode";
import { BUILD } from "../buildenv";
import { resourceProtocol } from "./real/resourceTiming";
import {
  authenticatedFetch,
  authEnabled,
  classifyAuthenticationFailure,
  csrfHeader,
} from "../auth";
import { transferStreamCount } from "./real/streamPolicy";
import { median } from "./stats";
import {
  needsPings,
  protocolFromNextHop,
  selectThroughputTarget,
  selectLatencyTarget,
  fetchViewOfOrigin,
  browserProtocolMatchesTarget,
  classifyTransportDiscovery,
  laneUrl,
  sessionDownloadUrl,
  PER_STREAM_BYTES,
  type LaneUrlSpec,
  ROUTES,
} from "./real/backendPure";
import { ridesSession, transportRunnable } from "./real/transports";
import {
  PreflightUnavailableError,
  TransportUnavailableError,
} from "./real/transportError";
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
import {
  ESTABLISH_BUDGET_MS,
  H3_PROBE_ATTEMPTS,
  H3_PROBE_DEADLINE_MS,
} from "./real/budgets";
import { IdleKeepalive, LatencyChannel } from "./real/latencyChannel";
import { UploadProgressChannel } from "./real/uploadProgress";
import { UploadPresentationBridge } from "./uploadPresentationBridge";

export { PreflightUnavailableError, TransportUnavailableError };

/** Enough for the server to open a lane and write it, small enough that the check costs nothing measurable. */
const WT_VERIFY_BYTES = 16 * 1024;

/* Widens `Probe`'s protocol field to fit `InfraInfo` carried over from an earlier probe. */
interface PathEvidence {
  clientIp: string;
  clientIpVersion: 4 | 6;
  clientIpSource: "socket" | "forwarded";
  protocolNegotiated: string;
  load?: { active: number; max: number };
}

export class RealBackend implements RunnerBackend {
  /** The core handle: push samples / emit / report failures + health through it. */
  #host: CoreHost | null = null;
  /** AbortController for in-flight fetches/streams; aborted in onAbort. */
  #abort: AbortController | null = null;
  /** The transport established for the active phase, for stall/fail reporting. */
  #activeTransport: TransportKind | null = null;
  /** Independent role bindings are frozen from probe until the next run. */
  #throughputTarget: FetchThroughputTarget | null = null;
  /** The same origin over WebTransport, when the server advertises it. */
  #wtThroughputTarget: WebTransportThroughputTarget | null = null;
  #latencyTarget: LatencyTarget | null = null;
  #streamPolicy: TransferStreamPolicy = { mode: "auto", count: 1 };
  #discoveryOrigin = "";
  #discoveryProtocol: string | undefined;
  #probeInfo: InfraInfo | null = null;
  /* Monotonic probe epoch. */
  #probeEpoch = 0;

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

  /* Never runs at the same time as #latency: stopped in onRunStart, restarted on run end. */
  #idle = new IdleKeepalive({
    host: () => this.#host!,
    throughputTarget: () => this.#throughputTarget,
    latencyTarget: () => this.#latencyTarget,
  });

  #disposed = false;
  /* False while hidden; the idle keepalive stays stopped so the browser can park the tab. */
  #background = true;

  attach(host: CoreHost): void {
    this.#host = host;
  }

  /* ================= PROBE ================= */
  /* Resolves `InfraInfo`: client address, server identity, negotiated protocols, engine version, pre-test ping. */
  async probe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo> {
    try {
      return await this.#runProbe(config, signal, role);
    } finally {
      // On a hidden page it must not stay: Chromium throttles a hidden page's worker timers to roughly once a minute.
      if (!this.#background) this.#idle.stop();
    }
  }

  async #runProbe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo> {
    const previous = this.#probeInfo;
    const epoch = ++this.#probeEpoch;
    if (role !== "throughput") this.#idle.stop();
    this.#throughputTarget = null;
    this.#wtThroughputTarget = null;
    this.#latencyTarget = null;
    this.#discoveryProtocol = undefined;

    const { pf, discovery } = await this.#fetchDiscovery(epoch, signal);
    try {
      // Carrying a role over is only sound while the server advertises the same targets it did last time.
      if (previous?.discoveryGeneration !== pf.generation) role = undefined;

      const selected = this.#selectThroughputRole(
        config,
        discovery,
        pf,
        previous,
        role,
      );
      const needsLatency = this.#selectLatencyRole(
        config,
        discovery,
        previous,
        role,
      );

      const { pathProbe, firstHopProtocol } = await this.#probeThroughputPath(
        selected,
        previous,
        role,
        signal,
      );
      this.#assertCurrentProbe(epoch);
      const latencyPathProbe = await this.#probeLatencyPath(
        previous,
        role,
        needsLatency,
        signal,
      );
      this.#assertCurrentProbe(epoch);

      if (needsLatency && role !== "throughput") {
        await this.#verifyLatencyChannel(discovery, config, signal);
        this.#assertCurrentProbe(epoch);
      }

      await this.#commitThroughputTransport(
        config,
        pf,
        previous,
        role,
        epoch,
        signal,
      );

      // Keepalive RTTs supply the pre-test ping median: RTT is client-measured, the server sends 0.
      const probeRtts =
        needsLatency && role !== "throughput"
          ? await this.#idle.collectRtts(signal)
          : [];
      this.#assertCurrentProbe(epoch);

      const info = this.#assembleInfra(pf, previous, selected, {
        throughput: pathProbe,
        latency: latencyPathProbe,
        firstHopProtocol,
        probeRtts,
      });
      info.discovery = discovery;
      this.#probeInfo = info;
      return info;
    } catch (cause) {
      if (cause instanceof TransportUnavailableError)
        cause.discovery = discovery;
      throw cause;
    }
  }

  /* Reads as an abort, which is what supersession is to the older caller. */
  #assertCurrentProbe(epoch: number): void {
    if (epoch !== this.#probeEpoch)
      throw new DOMException("probe superseded", "AbortError");
  }

  /* Records the origin and negotiated protocol the page itself reached. */
  async #fetchDiscovery(
    epoch: number,
    signal?: AbortSignal,
  ): Promise<{ pf: Preflight; discovery: TransportDiscovery }> {
    let pf: Preflight;
    let origin: string;
    let nextHopProtocol: string | undefined;
    try {
      // A logical server may restart with different targets while the SPA remains open, so each run resolves discovery.
      const ident = `?client=web&client_version=${encodeURIComponent(BUILD.clientVersion)}`;
      const res = await authenticatedFetch(`/preflight${ident}`, {
        method: "GET",
        cache: "no-store",
        signal,
      });
      if (!res.ok) throw new Error(`preflight returned HTTP ${res.status}`);
      pf = parsePreflight(await readJSONResponse(res));
      origin = new URL(res.url, location.href).origin;
      // Resource Timing exposes nextHopProtocol cross-origin only when the response carries Timing-Allow-Origin.
      nextHopProtocol = (
        performance.getEntriesByName(res.url, "resource").at(-1) as
          PerformanceResourceTiming | undefined
      )?.nextHopProtocol;
    } catch (cause) {
      await classifyAuthenticationFailure(signal);
      throw new PreflightUnavailableError("preflight unavailable", { cause });
    }
    this.#assertCurrentProbe(epoch);
    this.#discoveryOrigin = origin;
    this.#discoveryProtocol = nextHopProtocol;
    const discovery = classifyTransportDiscovery(
      pf.capabilities.throughput,
      pf.capabilities.latency,
      this.#discoveryOrigin,
      location.protocol === "https:",
      this.#discoveryProtocol,
    );
    Object.assign(discovery, {
      generation: pf.generation,
      engineVersion: pf.engineVersion,
      server: pf.server,
      fetchedAt: Date.now(),
    });
    return { pf, discovery };
  }

  /* A session target is separate; the fetch view carries fallback bytes and proves the path. */
  #selectThroughputRole(
    config: RunnerConfig,
    discovery: TransportDiscovery,
    pf: Preflight,
    previous: InfraInfo | null,
    role?: ConnectionRole,
  ): FetchThroughputTarget {
    const selection = config.transports.throughputTarget;
    // Resolve throughput before support checks; latency selection defaults to the browser's capability filter.
    const advertisedTarget = selectThroughputTarget(discovery, selection, true);
    if (!advertisedTarget)
      throw new TransportUnavailableError(`${selection} target unavailable`, {
        role: "throughput",
      });
    if (advertisedTarget.transport !== "fetch-stream") {
      if (!transportRunnable(advertisedTarget.transport))
        throw new TransportUnavailableError(
          `${advertisedTarget.transport} is not supported by this client`,
          { role: "throughput" },
        );
      this.#wtThroughputTarget = advertisedTarget;
    } else {
      this.#wtThroughputTarget = null;
    }
    const selected =
      advertisedTarget.transport !== "fetch-stream"
        ? { ...fetchViewOfOrigin(discovery, advertisedTarget) }
        : { ...advertisedTarget };
    if (
      role === "latency" &&
      selected.protocol === "negotiated" &&
      previous?.discoveryGeneration === pf.generation &&
      previous.selectedThroughputTarget === selected.id &&
      previous.selectedThroughputProtocol &&
      previous.selectedThroughputProtocol !== "negotiated"
    )
      selected.protocol = previous.selectedThroughputProtocol;
    this.#throughputTarget = selected;
    return selected;
  }

  /* Bind the latency role and report whether the run needs it for latency or transfer stages. */
  #selectLatencyRole(
    config: RunnerConfig,
    discovery: TransportDiscovery,
    previous: InfraInfo | null,
    role?: ConnectionRole,
  ): boolean {
    // Reuse the committed latency target; this probe does not verify it and must not undo an earlier fallback.
    const committed =
      role === "throughput" && previous?.selectedLatencyTarget
        ? selectLatencyTarget(
            discovery,
            previous.selectedLatencyTarget,
            transportRunnable("webtransport"),
          )
        : null;
    this.#latencyTarget =
      committed ??
      selectLatencyTarget(
        discovery,
        config.transports.latencyTarget,
        transportRunnable("webtransport"),
      );
    const needsLatency =
      config.stages.latency ||
      (!config.skipLoadedLatencyWhenStageOff &&
        (config.stages.download ||
          config.stages.upload ||
          config.stages.bidirectional));
    if (needsLatency && role !== "throughput" && !this.#latencyTarget)
      throw new TransportUnavailableError(
        `${config.transports.latencyTarget} latency target unavailable`,
        { role: "latency" },
      );
    if (!needsLatency) this.#latencyTarget = null;
    return needsLatency;
  }

  /* `GET {path}/probe` over the fetch view, which proves the path and the protocol the browser negotiated on it. */
  async #probeThroughputPath(
    selected: FetchThroughputTarget,
    previous: InfraInfo | null,
    role: ConnectionRole | undefined,
    signal?: AbortSignal,
  ): Promise<{ pathProbe: PathEvidence; firstHopProtocol?: string }> {
    const attempts = selected.protocol === "http3" ? H3_PROBE_ATTEMPTS : 1;
    const deadline = performance.now() + H3_PROBE_DEADLINE_MS;
    let pathProbe: PathEvidence | null =
      role === "latency" && previous
        ? {
            clientIp: previous.clientIp,
            clientIpVersion: previous.clientIpVersion,
            clientIpSource: previous.clientIpSource,
            protocolNegotiated: previous.protocolNegotiated,
            // Occupancy belongs to the throughput probe; a latency-only recheck learns nothing new.
            load: previous.serverLoad,
          }
        : null;
    let firstHopProtocol =
      role === "latency" ? previous?.firstHopProtocol : undefined;
    if (role !== "latency") {
      const probeCtl = new AbortController();
      const probeSignal = signal
        ? AbortSignal.any([signal, probeCtl.signal])
        : probeCtl.signal;
      const probeDeadline =
        selected.protocol === "http3"
          ? setTimeout(() => probeCtl.abort(), H3_PROBE_DEADLINE_MS)
          : undefined;
      try {
        for (
          let attempt = 0;
          attempt < attempts && performance.now() < deadline;
          attempt++
        ) {
          const probeURL = `${selected.origin}${selected.routes.probe}?cb=${performance.now()}-${attempt}`;
          const probeRes = await authenticatedFetch(probeURL, {
            cache: "no-store",
            signal: probeSignal,
          });
          if (!probeRes.ok)
            throw new Error(`probe returned HTTP ${probeRes.status}`);
          pathProbe = parseProbe(await readJSONResponse(probeRes));
          firstHopProtocol = await resourceProtocol(probeRes.url, probeSignal);
          if (
            selected.protocol !== "http3" ||
            browserProtocolMatchesTarget(selected, firstHopProtocol)
          )
            break;
        }
      } catch (cause) {
        await classifyAuthenticationFailure(probeSignal);
        if (selected.protocol === "http3")
          throw new TransportUnavailableError("http3 transport unavailable", {
            cause,
            role: "throughput",
          });
        throw new TransportUnavailableError("throughput probe request failed", {
          cause,
          role: "throughput",
        });
      } finally {
        if (probeDeadline !== undefined) clearTimeout(probeDeadline);
      }
      // The fetch view carries fallback bytes, so its protocol is proven even when a session is committed.
      const fetchProtocolProven = browserProtocolMatchesTarget(
        selected,
        firstHopProtocol,
      );
      if (!pathProbe || (!fetchProtocolProven && !this.#wtThroughputTarget))
        throw new TransportUnavailableError(
          `${selected.protocol} transport unavailable`,
          { role: "throughput" },
        );
      // An unproven protocol would otherwise pick the stream policy of one the browser never negotiated.
      if (selected.protocol === "negotiated" || !fetchProtocolProven)
        selected.protocol =
          protocolFromNextHop(firstHopProtocol) ?? "negotiated";
    }
    if (!pathProbe)
      throw new TransportUnavailableError("throughput evidence unavailable", {
        role: "throughput",
      });
    return { pathProbe, firstHopProtocol };
  }

  /* Probe the latency target's path; it may resolve a different client address than throughput. */
  async #probeLatencyPath(
    previous: InfraInfo | null,
    role: ConnectionRole | undefined,
    needsLatency: boolean,
    signal?: AbortSignal,
  ): Promise<PathEvidence | undefined> {
    let latencyPathProbe: PathEvidence | undefined =
      role === "throughput" && previous?.latencyClientIp
        ? {
            clientIp: previous.latencyClientIp,
            clientIpVersion: previous.latencyClientIpVersion!,
            clientIpSource: previous.latencyClientIpSource!,
            protocolNegotiated: previous.latencyProtocolNegotiated!,
          }
        : undefined;
    if (needsLatency && this.#latencyTarget && role !== "throughput") {
      const latencyURL = `${this.#latencyTarget.origin}${this.#latencyTarget.routes.probe}?cb=${performance.now()}`;
      let latencyRes: Response;
      try {
        latencyRes = await authenticatedFetch(latencyURL, {
          cache: "no-store",
          signal,
        });
        if (!latencyRes.ok)
          throw new Error(`latency probe returned HTTP ${latencyRes.status}`);
        latencyPathProbe = parseProbe(await readJSONResponse(latencyRes));
      } catch (cause) {
        await classifyAuthenticationFailure(signal);
        throw new TransportUnavailableError("latency probe request failed", {
          cause,
          role: "latency",
        });
      }
    }
    return latencyPathProbe;
  }

  /* Decide whether the run carries bytes over a session or over fetch. */
  async #commitThroughputTransport(
    config: RunnerConfig,
    pf: Preflight,
    previous: InfraInfo | null,
    role: ConnectionRole | undefined,
    epoch: number,
    signal?: AbortSignal,
  ): Promise<void> {
    const throughputAlreadyCommitted =
      this.#wtThroughputTarget !== null &&
      role === "latency" &&
      previous?.discoveryGeneration === pf.generation;
    if (throughputAlreadyCommitted) {
      if (previous!.selectedThroughputTransport === "fetch-stream")
        this.#wtThroughputTarget = null;
      return;
    }
    if (!this.#wtThroughputTarget) return;
    // An advertised WebTransport target still needs UDP to reach the server, so the run commits to what a dial proves.
    const verdict = await this.#verifyWtThroughput(signal);
    // The dial is the longest await; assert the epoch before mutating this probe's backend state.
    this.#assertCurrentProbe(epoch);
    if (verdict.ok) return;
    // An explicit selection fails for its role; automatic selection degrades.
    const selection = config.transports.throughputTarget;
    if (selection !== "auto" && selection !== "current")
      throw new TransportUnavailableError(verdict.detail, {
        role: "throughput",
      });
    this.#wtThroughputTarget = null;
  }

  /** Fold the run's evidence into the InfraInfo the UI reads. */
  #assembleInfra(
    pf: Preflight,
    previous: InfraInfo | null,
    selected: FetchThroughputTarget,
    evidence: {
      throughput: PathEvidence;
      latency?: PathEvidence;
      firstHopProtocol?: string;
      probeRtts: number[];
    },
  ): InfraInfo {
    const { throughput, latency, firstHopProtocol, probeRtts } = evidence;
    return {
      clientIp: throughput.clientIp,
      clientIpVersion: throughput.clientIpVersion,
      clientIpSource: throughput.clientIpSource,
      latencyClientIp: latency?.clientIp,
      latencyClientIpVersion: latency?.clientIpVersion,
      latencyClientIpSource: latency?.clientIpSource,
      server: {
        name: pf.server.name,
        location: pf.server.location,
      },
      preTestPingMs: probeRtts.length
        ? median(probeRtts)
        : (previous?.preTestPingMs ?? 0),
      engineVersion: pf.engineVersion,
      discoveryGeneration: pf.generation,
      protocolNegotiated: throughput.protocolNegotiated,
      selectedThroughputTarget: this.#wtThroughputTarget?.id ?? selected.id,
      selectedThroughputProtocol: selected.protocol,
      selectedThroughputTransport:
        this.#wtThroughputTarget?.transport ?? "fetch-stream",
      selectedLatencyTarget: this.#latencyTarget?.id,
      selectedLatencyTransport: this.#latencyTarget?.transport,
      latencyProtocolNegotiated: latency?.protocolNegotiated,
      firstHopProtocol,
      firstHopSecure: selected.tls,
      serverLoad: throughput.load,
    };
  }

  /* What this engine can drive, per role: WebSocket and WebTransport pings, fetch-stream and WebTransport transfer. */
  describe(): EngineInfo {
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
    // Pause idle keepalive; the stage-owned ping channel owns latency, and #closeAll restarts it afterward.
    this.#idle.stop();
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
      const kind =
        activity.stage === "latency"
          ? (this.#latencyTarget?.transport ?? null)
          : (this.#wtThroughputTarget?.transport ??
            (this.#throughputTarget ? "fetch-stream" : null));
      if (!kind) {
        this.#host!.failStage(
          activity.stage,
          "transport-unavailable",
          "server offers no supported transfer transport",
        );
        return;
      }
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

  /* A handshake proves reachability; opening a stream and reading a byte proves the selected transfer path. */
  async #verifyWtThroughput(
    signal?: AbortSignal,
  ): Promise<{ ok: boolean; detail: string }> {
    const wt = this.#wtThroughputTarget;
    if (!wt) return { ok: false, detail: "no webtransport target resolved" };
    let established = false;
    try {
      let url = `${wt.origin}${wt.routes.wtDownload}?bytes=${WT_VERIFY_BYTES}`;
      if (authEnabled) {
        const minted = await authenticatedFetch(
          `${wt.origin}${wt.routes.wtSession}`,
          {
            method: "POST",
            cache: "no-store",
            signal,
            headers: csrfHeader(),
          },
        );
        // A refused mint is not a transport verdict: without its token, dial failure says nothing about UDP.
        if (!minted.ok)
          return {
            ok: false,
            detail: `webtransport token mint refused (${minted.status})`,
          };
        const token = parseResponseToken(
          await readJSONResponse(minted),
          "token",
        );
        url += `&token=${encodeURIComponent(token)}`;
      }
      const session = new WebTransport(url);
      // A session that never establishes rejects `closed` as well as `ready`.
      void session.closed.catch(() => {});
      const close = (): void => session.close();
      signal?.addEventListener("abort", close, { once: true });
      // One deadline covers handshake and lane: the run needs bytes, not a session that serves nothing.
      const deadline = setTimeout(close, ESTABLISH_BUDGET_MS);
      try {
        await session.ready;
        established = true;
        const lanes = session.incomingUnidirectionalStreams.getReader();
        const lane = await lanes.read();
        if (lane.done) throw new Error("no lane");
        const chunk = await (lane.value as ReadableStream<Uint8Array>)
          .getReader()
          .read();
        if (chunk.done) throw new Error("empty lane");
      } finally {
        clearTimeout(deadline);
        signal?.removeEventListener("abort", close);
        // Every exit closes the session; otherwise a GC-retained session keeps its server admission slot.
        close();
      }
      return { ok: true, detail: "" };
    } catch (cause) {
      // A superseded probe reports no verdict; aborts must not write backend-wide state.
      if (signal?.aborted) throw cause;
      // A session that came up and then carried nothing is a different fault from one that never reached the server.
      return {
        ok: false,
        detail: established
          ? "webtransport session carried no bytes"
          : "webtransport session did not establish",
      };
    }
  }

  /* ================= TRANSPORT NEGOTIATION ================= */
  /* An advertised WebTransport target still needs UDP to reach the server, so a failure reselects the WebSocket. */
  async #verifyLatencyChannel(
    discovery: TransportDiscovery,
    config: RunnerConfig,
    signal?: AbortSignal,
  ): Promise<void> {
    try {
      await this.#idle.verifyReady(signal);
      return;
    } catch (error) {
      if (this.#latencyTarget?.transport !== "webtransport") throw error;
      // An explicit WT selection fails rather than silently changing transport.
      if (config.transports.latencyTarget !== "auto") throw error;
    }
    this.#idle.stop();
    this.#latencyTarget = selectLatencyTarget(
      discovery,
      config.transports.latencyTarget,
      false,
    );
    if (!this.#latencyTarget)
      throw new TransportUnavailableError(
        "WebTransport latency channel did not establish",
        { role: "latency" },
      );
    await this.#idle.verifyReady(signal);
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
    const base = this.#throughputTarget!.origin;
    const streams = transferStreamCount({
      protocol: this.#throughputTarget!.protocol,
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
      downloadPath: this.#throughputTarget?.routes.download ?? ROUTES.download,
      uploadPath: this.#throughputTarget?.routes.upload ?? ROUTES.upload,
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
      if (!this.#transferActive || !this.#lanes[dir]) return;
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
    const path =
      this.#throughputTarget?.routes.uploadSession ?? ROUTES.uploadSession;
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
    this.#idle.stop();
    this.#discardTransfer();
    this.#latency.teardown();
    this.#stalled = false;
    if (this.#abort) {
      this.#abort.abort();
      this.#abort = null;
    }
    this.#activeTransport = null;
    // Resume idle keepalive so connectivity stays live instead of freezing until the next probe or run.
    if (!this.#disposed && this.#background) this.#idle.start();
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

  /* Suspend the idle keepalive while the page is hidden. */
  setBackgroundActivity(enabled: boolean): void {
    if (this.#background === enabled) return;
    this.#background = enabled;
    if (this.#disposed) return;
    if (enabled) this.#idle.start();
    else this.#idle.stop();
  }

  dispose(): void {
    this.#disposed = true;
    this.#closeAll();
  }
}
