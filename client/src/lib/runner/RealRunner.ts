// Real measurement backend: negotiates browser transports, drives the transfer
// lanes and the latency/upload-progress channels, and pushes only measured wire
// samples into RunnerCore.
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
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../api/endpoints";
import type { Preflight } from "../api/preflight";
import type { Probe } from "../api/probe";
import { debugEnabled } from "../debug";
import { BUILD } from "../buildenv";
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
import { TransportUnavailableError } from "./real/transportError";
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

export { TransportUnavailableError };

/** Enough for the server to open a lane and write it, small enough that the
 *  check costs nothing measurable. */
const WT_VERIFY_BYTES = 16 * 1024;

/** What `GET {path}/probe` proves about one role's path. Widens the generated
 *  `Probe` shape's protocol field, which an InfraInfo carried over from an
 *  earlier probe also has to fit. */
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
  /** Monotonic probe epoch. A probe can be superseded by a newer one while its
   *  awaits are pending, and the role bindings are backend-wide: the older body
   *  must not write state the newer one already committed. */
  #probeEpoch = 0;

  /* ---- transfer stage state ----
   *  Bidirectional primes BOTH directions on the SAME stage, calling
   *  #primeTransfer once per activity.transfer entry, so this is keyed by
   *  FlowDirection. A standalone download/upload stage fills exactly one entry. */
  /** One lane pool + its bookkeeping, per active transfer direction. */
  #lanes: Partial<Record<FlowDirection, TransferDirection>> = {};
  /** True from the stage's first #primeTransfer to #teardownTransfer. Both
   *  directions are primed and torn down together. */
  #transferActive = false;
  /** The STAGE-level stalled flag reported to the host, deduped so stall/resume
   *  fire once per edge. #reconcileStall or the idle ping channel latches it. */
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
    setLaneStalled: (stalled, detail) =>
      this.#lanes.up?.setStalled(stalled, detail),
  });

  /** What every transfer direction of the stage is given. */
  #directionHost: DirectionHost = {
    host: () => this.#host!,
    sampleProvesStageLiveness: () => !this.#stalled,
    stallChanged: (detail) => this.#reconcileStall(detail),
    uploadProgress: (msg, generation) =>
      this.#uploadProgress.accept(msg, generation),
    beginUploadMeasure: () => this.#uploadProgress.beginMeasure(),
    discardTransfer: () => this.#discardTransfer(),
  };

  /** The stage-owned ping channel. Its stall/resume reach the core ONLY for the
   *  idle latency stage; during a transfer stage the byte lanes drive link
   *  health, so loaded-latency reconnects pass silently. */
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

  /** The connectivity/preflight keepalive. Never runs at the same time as
   *  #latency: stopped in onRunStart, restarted on run end. */
  #idle = new IdleKeepalive({
    host: () => this.#host!,
    throughputTarget: () => this.#throughputTarget,
    latencyTarget: () => this.#latencyTarget,
  });

  #disposed = false;
  /** False while the page is hidden: the idle keepalive stays stopped so the
   *  browser can park the tab, and a probe that ran while hidden parks it again
   *  on its way out. A run overrides it, since starting one is a deliberate
   *  foreground act. */
  #background = true;

  attach(host: CoreHost): void {
    this.#host = host;
  }

  /* ================= PROBE ================= */
  /**
   * Same-origin `GET /preflight`, then the per-role path probes. Resolves
   * `InfraInfo`: client address, server identity, negotiated protocols, engine
   * version, pre-test ping. Emits pre-test `latency` samples (negative `t`) for
   * the sparkline. Throws, which engine.svelte.ts maps to `preflight-failed`.
   */
  async probe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo> {
    try {
      return await this.#runProbe(config, signal, role);
    } finally {
      // The probe starts the keepalive for its RTTs and leaves it up to drive
      // the connectivity pill. On a hidden page it must not stay: Chromium
      // throttles a hidden page's worker timers to roughly once a minute after
      // five minutes, well outside the server's idle bound, so the bus would be
      // reaped and reconnected on a loop nobody is watching. Parking it here
      // keeps the tab parkable whichever event started the probe (an `online`
      // edge, or boot in a background tab); setBackgroundActivity brings the
      // keepalive back when the page is visible again.
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
    // Carrying a role over is only sound while the server advertises the same
    // targets it did last time.
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

    // Keepalive RTTs supply the pre-test ping median: RTT is client-measured,
    // the server sends 0. A ping failure must not fail preflight.
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
    this.#probeInfo = info;
    return info;
  }

  /** Stop a superseded probe at its next await boundary, so it cannot write a
   *  role binding or #probeInfo over what a newer probe already committed.
   *  Reads as an abort, which is what supersession is to the older caller. */
  #assertCurrentProbe(epoch: number): void {
    if (epoch !== this.#probeEpoch)
      throw new DOMException("probe superseded", "AbortError");
  }

  /** Resolve the discovery document this run selects its roles from, and emit
   *  it. Records the origin and negotiated protocol the page itself reached.
   *  Nothing outside the fetch runs before the epoch assert: the two fields are
   *  backend-wide, and a superseded emit re-opens the validation loop the newer
   *  probe just closed (engine.svelte.ts reacts to a generation change by
   *  clearing the prepared selection and marking both roles stale). */
  async #fetchDiscovery(
    epoch: number,
    signal?: AbortSignal,
  ): Promise<{ pf: Preflight; discovery: TransportDiscovery }> {
    let pf: Preflight;
    let origin: string;
    let nextHopProtocol: string | undefined;
    try {
      // A logical server may restart with different public targets while the
      // SPA remains open, so every run resolves a fresh discovery document.
      const ident = `?client=web&client_version=${encodeURIComponent(BUILD.clientVersion)}`;
      const res = await authenticatedFetch(`/preflight${ident}`, {
        method: "GET",
        cache: "no-store",
        signal,
      });
      if (!res.ok) throw new Error(`preflight returned HTTP ${res.status}`);
      pf = (await res.json()) as Preflight;
      origin = new URL(res.url, location.href).origin;
      // Resource Timing exposes nextHopProtocol cross-origin only when the
      // response carries Timing-Allow-Origin.
      nextHopProtocol = (
        performance.getEntriesByName(res.url, "resource").at(-1) as
          PerformanceResourceTiming | undefined
      )?.nextHopProtocol;
    } catch (cause) {
      await classifyAuthenticationFailure(signal);
      throw new Error(`preflight request failed: ${String(cause)}`, { cause });
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
    this.#host?.emit({ type: "transportDiscovery", discovery });
    return { pf, discovery };
  }

  /** Bind the throughput role and return the fetch view of it. A session target
   *  is held separately: the fetch view is still what a fallback carries bytes
   *  over, so it is the one the path probe proves. */
  #selectThroughputRole(
    config: RunnerConfig,
    discovery: TransportDiscovery,
    pf: Preflight,
    previous: InfraInfo | null,
    role?: ConnectionRole,
  ): FetchThroughputTarget {
    const selection = config.transports.throughputTarget;
    // Unfiltered on purpose, which is why this parameter defaults on where
    // `selectLatencyTarget`'s defaults off: resolving first and refusing below
    // names the mechanism. Passing the browser's real capability here — the
    // symmetry the two calls in #selectLatencyRole invite — would return null
    // for a WebTransport-only origin and degrade that to "target unavailable".
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

  /** Bind the latency role, and report whether this run needs one at all: the
   *  latency stage, or a transfer stage measuring loaded latency. A
   *  throughput-role probe reuses what the latency role committed to, the way
   *  it reuses that role's /probe evidence. */
  #selectLatencyRole(
    config: RunnerConfig,
    discovery: TransportDiscovery,
    previous: InfraInfo | null,
    role?: ConnectionRole,
  ): boolean {
    // A throughput-role probe does not run #verifyLatencyChannel, so it must
    // not re-select either: the check may have degraded off the transport the
    // selector prefers, and re-running the selector would rebind the run to a
    // bus already proven dead. The caller clears `role` unless the generation
    // still matches, so a carried id names a target this discovery advertises.
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

  /** `GET {path}/probe` over the fetch view, which proves the path and the
   *  protocol the browser negotiated on it. Narrows `selected.protocol` to what
   *  the first hop reports. A latency-role probe reuses the throughput role's
   *  evidence instead of re-running the request. */
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
            // Occupancy is the throughput probe's, and a latency-only recheck
            // learns nothing new about it. Dropped, the endpoint panel's slots
            // row silently disappears on every such recheck.
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
          pathProbe = (await probeRes.json()) as Probe;
          const timing = performance
            .getEntriesByName(probeRes.url, "resource")
            .at(-1) as PerformanceResourceTiming | undefined;
          firstHopProtocol = timing?.nextHopProtocol || undefined;
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
      // The fetch view is what a fallback would carry bytes over, so its
      // protocol is proven even when a session is committed. A mismatch fails
      // the role only when nothing else can carry them.
      const fetchProtocolProven = browserProtocolMatchesTarget(
        selected,
        firstHopProtocol,
      );
      if (!pathProbe || (!fetchProtocolProven && !this.#wtThroughputTarget))
        throw new TransportUnavailableError(
          `${selected.protocol} transport unavailable`,
          { role: "throughput" },
        );
      // An unproven protocol would otherwise pick the stream policy of one the
      // browser never negotiated.
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

  /** `GET {path}/probe` on the latency target's own path, which may resolve a
   *  different client address from the throughput one. A throughput-role probe
   *  carries the last latency evidence over. */
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
        latencyPathProbe = (await latencyRes.json()) as Probe;
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

  /** Decide whether the run carries bytes over a session or over fetch. A
   *  latency-role probe reuses what the throughput role committed to, the way
   *  it reuses that role's /probe evidence and negotiated protocol. */
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
    // An advertised WebTransport target still needs UDP to reach the server,
    // so the run commits to what a dial proves.
    const verdict = await this.#verifyWtThroughput(signal);
    // The dial is the longest await in the probe, so a newer probe may have
    // bound its own targets by now; degrading here would degrade that one.
    this.#assertCurrentProbe(epoch);
    if (verdict.ok) return;
    // An explicit selection fails its role loudly; automatic degrades.
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

  /** What this engine can drive, per role: WebSocket and WebTransport pings,
   *  fetch-stream and WebTransport transfer. Each engine reports its own. */
  describe(): EngineInfo {
    return {
      name: "real",
      version: BUILD.clientVersion, // built with the client
      latencyTransports: transportRunnable("webtransport")
        ? ["webtransport", "websocket"]
        : ["websocket"],
      // Preference order, which for throughput leads with fetch: streams over
      // TCP still win raw rate, so a session is the explicit choice.
      throughputTransports: transportRunnable("webtransport")
        ? ["fetch-stream", "webtransport", "webtransport-datagram"]
        : ["fetch-stream"],
    };
  }

  /* ================= LIFECYCLE (core → backend) ================= */
  /**
   * A run is starting. Opens a fresh AbortController so onAbort can cancel
   * everything, and resets per-run state. Transfer connections open per stage,
   * in onStageBegin.
   */
  onRunStart(config: RunnerConfig): void {
    // Pause the idle keepalive: the stage-owned ping channel owns latency for
    // the run, and #closeAll restarts it on completion or abort.
    this.#idle.stop();
    this.#streamPolicy = { ...config.transferStreams };
    this.#abort = new AbortController();
    this.#activeTransport = null;
    this.#discardTransfer(); // discard leftovers from a prior run
    this.#stalled = false;
    // Unique-per-run cache-buster off the monotonic clock, not wall time; the
    // stream index is appended per worker.
    this.#cbSeed = `r${Math.round(performance.now())}`;
  }

  onStageBegin(activity: PhaseActivity): void | Promise<void> {
    const preparations: Promise<void>[] = [];
    // The ping channel is ALWAYS a latency-role transport on its OWN socket.
    // Negotiate it first, so #activeTransport ends as the lanes' transfer kind.
    if (needsPings(activity)) {
      const pingKind = this.#committedKind("latency");
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
      const kind = this.#committedKind(activity.stage);
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

  /**
   * The stage's warmup window has elapsed and its connections are warm. Push
   * real samples on the SAME connections; reopening them discards the warmup.
   * Fires immediately after onStageBegin when warmupMs <= 0. `underLoad` marks
   * pings taken while the stage moves bytes (bufferbloat).
   */
  onStageMeasure(activity: PhaseActivity): void {
    const underLoad = activity.transfer.length > 0;
    // A direction missing here failed to prime, and has nothing to measure.
    for (const dir of activity.transfer) this.#lanes[dir]?.measure();
    if (needsPings(activity)) this.#latency.measure(underLoad);
  }

  /** A measured stage is over. Drains its authoritative boundary sample so the
   *  core reduces a complete result, then releases its connections. */
  onStageEnd(_activity: PhaseActivity, flush = true): void | Promise<void> {
    if (!flush) {
      this.#discardTransfer();
      this.#latency.teardown();
      // The stall belonged to the stage that just ended. Latched past it, it
      // gates sampleProvesStageLiveness for every later stage, whose bytes then
      // never refresh the core's watchdog: a healthy stage runs to the max-stall
      // timeout and its measurement is discarded.
      this.#stalled = false;
      return;
    }
    return this.#teardownTransfer().then(() => {
      this.#latency.teardown();
      this.#stalled = false;
    });
  }

  /** The run is complete. Close anything still open. */
  onComplete(): void {
    this.#closeAll();
  }

  /** The user aborted. Cancel in-flight fetches/streams and close sockets. The
   *  core flips to "aborted" and emits the transition; do not emit here. */
  onAbort(): void {
    this.#closeAll();
  }

  /** Token mint the WebTransport workers call before each dial. Minting is a
   *  measurement mutation, so it carries the CSRF header and credentials. */
  #wtMint(origin: string, route: string): SessionLaneOptions["mint"] {
    if (!authEnabled) return undefined;
    return {
      url: `${origin}${route}`,
      headers: csrfHeader(),
      credentials: "include",
    };
  }

  /** Dial a session and read a lane under one deadline. A handshake only proves
   *  the path reaches the server; the run's first act is opening a stream, so
   *  the check does that too rather than reporting a path its first request
   *  would fail on. */
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
        // A refused mint is not a transport verdict: without the token the
        // dial would fail for a reason that says nothing about UDP, so it is
        // reported as itself rather than as an unreachable transport.
        if (!minted.ok)
          return {
            ok: false,
            detail: `webtransport token mint refused (${minted.status})`,
          };
        const body = (await minted.json()) as { token?: unknown };
        if (typeof body.token === "string" && body.token !== "")
          url += `&token=${encodeURIComponent(body.token)}`;
      }
      const session = new WebTransport(url);
      // A session that never establishes rejects `closed` as well as `ready`.
      void session.closed.catch(() => {});
      const close = (): void => session.close();
      signal?.addEventListener("abort", close, { once: true });
      // One deadline covers the handshake and the lane: what the run needs is
      // bytes arriving, not a session that established and then served nothing.
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
        // Every exit path releases the session. A lane that never opens throws
        // between `ready` and the first byte, and an established session held
        // to GC keeps its server admission slot; probe() re-dials on every
        // draft change, visibility return and run start.
        close();
      }
      return { ok: true, detail: "" };
    } catch (cause) {
      // A superseded probe reports no verdict: every other await here throws on
      // abort, and the caller writes backend-wide role state after this returns.
      if (signal?.aborted) throw cause;
      // A session that came up and then carried nothing is a different fault
      // from one that never reached the server, and reads as one.
      return {
        ok: false,
        detail: established
          ? "webtransport session carried no bytes"
          : "webtransport session did not establish",
      };
    }
  }

  /* ================= TRANSPORT NEGOTIATION ================= */
  /** Prove the selected ping bus answers. An advertised WebTransport target
   *  still needs UDP to reach the server, so a failure reselects the WebSocket
   *  target once before the run commits. */
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

  /** The kind this role committed to at probe time, or null when nothing
   *  resolved. Selection happened there; a stage only reads the outcome. */
  #committedKind(role: TransportRole): TransportKind | null {
    if (role === "latency") return this.#latencyTarget?.transport ?? null;
    if (this.#wtThroughputTarget) return this.#wtThroughputTarget.transport;
    return this.#throughputTarget ? "fetch-stream" : null;
  }

  /* ================= PRIME (warmup window): open, don't measure ================= */
  /** Resolve one direction's transfer streams from the frozen protocol target
   *  and the run's automatic/forced policy. */
  #streamCount(activity: PhaseActivity, dir: FlowDirection): number {
    if (!this.#throughputTarget)
      throw new Error("transfer target not resolved");
    return transferStreamCount({
      protocol: this.#throughputTarget.protocol,
      policy: this.#streamPolicy,
      transfer: activity.transfer,
      dir,
      needsPing: needsPings(activity),
      webTransport: this.#wtThroughputTarget !== null,
    });
  }

  /** Open the resolved transfer stream(s) for `dir` over `kind` (`GET
   *  {path}/download?bytes=N` for "down", a streamed `POST {path}/upload` body
   *  for "up") and run priming bytes to warm the path (TCP congestion window /
   *  BBR / TLS), pushing NOTHING into the core. The measure step reuses them. */
  #primeTransfer(
    kind: TransportKind,
    dir: FlowDirection,
    activity: PhaseActivity,
  ): void | Promise<void> {
    // #committedKind returns a session kind only with a session target
    // resolved, so a session-borne kind always finds one here.
    const wt = ridesSession(kind) ? this.#wtThroughputTarget : null;

    // A stage names each direction once (bidirectional calls this twice, one per
    // direction): a duplicate call for the SAME direction is a real bug.
    if (this.#lanes[dir]) throw new Error(`duplicate ${dir} prime`);

    const cfg = this.#host!.config!;
    const base = this.#throughputTarget!.origin;
    const streams = this.#streamCount(activity, dir);
    // A WebTransport session is one worker whichever way it is measured: it
    // opens `streams` lanes internally and reports them as one.
    const laneCount = wt ? 1 : streams;
    // Experimental: the download worker requests adaptive chunks itself, so omit the
    // baked-in ?bytes= and let it append &bytes=N per fetch (see download-worker.ts).
    const chunkDownload = dir === "down" && cfg.experimentalChunkedDownload;

    const direction: TransferDirection = new TransferDirection({
      dir,
      stage: activity.stage,
      laneCount,
      warmupMs: cfg.duration.warmupMs,
      host: this.#directionHost,
      // Replaced by the session factory when this direction rides one.
      lane: (i, events) =>
        fetchLane(
          {
            dir,
            url: direction.streamUrls[i],
            lanes: laneCount,
            index: i,
            debug: debugEnabled(),
            credentials: authEnabled ? "include" : "same-origin",
            // CSRF on the upload POST only: on the download GET it makes a
            // cross-port transfer CORS-preflighted, costing a round trip.
            headers: dir === "up" ? csrfHeader() : {},
            chunk: chunkDownload,
          },
          events,
        ),
    });
    this.#lanes[dir] = direction;
    this.#transferActive = true;

    // Download streams the body down (?bytes=N sizes it); upload streams a
    // generated body up until the stage stops, keyed by a per-stage id.
    const spec: LaneUrlSpec = {
      dir,
      base,
      downloadPath: this.#throughputTarget?.routes.download ?? ROUTES.download,
      uploadPath: this.#throughputTarget?.routes.upload ?? ROUTES.upload,
      cbSeed: this.#cbSeed,
      bytes: PER_STREAM_BYTES,
      chunkDownload,
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
    // Workers start immediately, warming the TCP cwnd. Warmup download progress
    // carries seq=0 and is ignored, so no warmup bytes bleed into measurement.
  }

  /** The live lane for `dir`, or null once a teardown or abort released it.
   *  Re-read after every await, so a released lane never spawns workers no one
   *  owns. */
  #liveLane(dir: FlowDirection): TransferDirection | null {
    const lane = this.#lanes[dir];
    return this.#transferActive && lane ? lane : null;
  }

  /** Mint the upload session id, establish the progress meter on it, then open
   *  the POST lanes or the session's upload lanes against that id. */
  async #primeUploadTransfer(
    dir: FlowDirection,
    base: string,
    laneCount: number,
    url: (i: number, uploadId?: string) => string,
    streams: number,
    wt: WebTransportThroughputTarget | null,
  ): Promise<void> {
    const primedLane = this.#lanes[dir];
    let id: string;
    try {
      id = await this.#mintUploadSession(base);
    } catch {
      if (!this.#liveLane(dir)) return; // aborted or torn down mid-request
      this.#host!.failStage(
        primedLane!.stage,
        "protocol-error",
        "upload session request failed",
      );
      return;
    }
    const uploadLane = this.#liveLane(dir);
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
      // The session worker reads the feed before its lanes write, so the
      // counter is already running when bytes start.
      uploadLane.newLane = (_i, events) => sessionLane(sessionOpts, events);
      uploadLane.spawn([sessionOpts.url]);
      if ((await feed) === "timeout") {
        this.#host!.failStage(
          uploadLane.stage,
          "connection-lost",
          "upload progress channel could not be established",
        );
      }
      return;
    }
    // The progress stream is the authoritative upload meter. Establish it ahead
    // of the POST workers, so forced H1 lanes cannot take every connection slot.
    if (!(await this.#uploadProgress.prime(uploadLane.stage, id))) return;
    const progressLane = this.#liveLane(dir);
    if (!progressLane) return;
    progressLane.setUploadGeneration(this.#uploadProgress.generation);
    progressLane.spawn(Array.from({ length: laneCount }, (_, i) => url(i, id)));
  }

  async #mintUploadSession(base: string): Promise<string> {
    const path =
      this.#throughputTarget?.routes.uploadSession ?? ROUTES.uploadSession;
    // Own deadline + the run's abort: fetch must reject within the timeout even
    // when the request hangs, so the stage skips instead of max-stalling.
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
      const body = (await res.json()) as { uploadId?: unknown };
      if (typeof body.uploadId !== "string" || body.uploadId === "") {
        throw new Error("upload session returned no uploadId");
      }
      return body.uploadId;
    } catch (cause) {
      await classifyAuthenticationFailure(ctl.signal);
      throw cause;
    } finally {
      clearTimeout(deadline);
      this.#abort?.signal.removeEventListener("abort", onRunAbort);
    }
  }

  /** Combine the directions into the STAGE-level flag. Every required lane must
   *  be healthy: one direction moving cannot validate its stalled sibling. */
  #reconcileStall(detail?: string): void {
    const transferStalled = transferStageStalled(
      Object.values(this.#lanes) as TransferDirection[],
    );
    if (transferStalled && !this.#stalled) {
      this.#host!.stall({
        reason: "connection-lost",
        transport: this.#activeTransport ?? undefined,
        detail,
      });
      this.#stalled = true;
    } else if (!transferStalled && this.#stalled) {
      this.#host!.resume();
      this.#stalled = false;
    }
  }

  /** Stop every POST lane, wait for the server's terminal upload count, then
   *  release the stage state. */
  async #teardownTransfer(): Promise<void> {
    // A graceful WT stop finalizes in the worker; the progress teardown then
    // has nothing left to wait on. For fetch, BYE must follow the POST lanes,
    // so the server's final count includes everything they drained.
    await Promise.all(Object.values(this.#lanes).map((d) => d!.stop()));
    await this.#uploadProgress.teardown(true);
    this.#transferActive = false;
    this.#lanes = {};
  }

  #discardTransfer(): void {
    for (const direction of Object.values(this.#lanes)) direction!.discard();
    void this.#uploadProgress.teardown(false);
    this.#transferActive = false;
    this.#lanes = {};
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
    // Resume the idle keepalive so the connectivity pill stays live instead of
    // freezing at its last-known state until the next probe or run.
    if (!this.#disposed && this.#background) this.#idle.start();
  }

  /** Suspend the idle keepalive while the page is hidden. Stopping it closes
   *  the ping socket and its worker, which is what lets the browser park the
   *  tab; resuming re-probes, so the pill reports a fresh edge. */
  setBackgroundActivity(enabled: boolean): void {
    if (this.#background === enabled) return;
    this.#background = enabled;
    if (this.#disposed) return;
    if (enabled) this.#idle.start();
    else this.#idle.stop();
  }

  dispose(): void {
    this.#disposed = true;
    this.#idle.stop();
    this.#discardTransfer();
    this.#latency.teardown();
    this.#abort?.abort();
    this.#abort = null;
    this.#activeTransport = null;
  }
}
