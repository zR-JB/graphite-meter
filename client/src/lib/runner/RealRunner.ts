// Real measurement backend: negotiates browser transports, drives the transfer
// lanes and the latency/upload-progress channels, and pushes only measured wire
// samples into RunnerCore.
import type {
  RunnerConfig,
  InfraInfo,
  EngineInfo,
  TransportKind,
  TransportRole,
  FlowDirection,
  PhaseActivity,
  TransferStreamPolicy,
  ConnectionRole,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/endpoints";
import type { Preflight } from "../api/preflight";
import type { Probe } from "../api/probe";
import { debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../debug";
import { BUILD } from "../buildenv";
import {
  authenticatedFetch,
  authEnabled,
  classifyAuthenticationFailure,
  csrfHeader,
  redirectToLogin,
} from "../auth";
import { transferStreamCount } from "./real/streamPolicy";
import {
  median,
  needsPings,
  laneStaggerMs,
  protocolFromNextHop,
  selectThroughputTarget,
  selectLatencyTarget,
  browserProtocolMatchesTarget,
  classifyTransportDiscovery,
  ROUTES,
} from "./real/backendPure";
import { TransportUnavailableError } from "./real/transportError";
import {
  downloadWorker,
  stopWorker,
  uploadWorker,
  type AuthRequiredMsg,
} from "./real/workerPool";
import { IdleKeepalive, LatencyChannel } from "./real/latencyChannel";
import { UploadProgressChannel } from "./real/uploadProgress";

export { TransportUnavailableError };

export interface RealBackendOptions {
  authToken?: string;
}

// Match the core/dummy cadence so both engines feed the UI at the same rate.
const THROUGHPUT_CADENCE_MS = 60;

// Large enough that a normal stage ends by aborting the stream, not by refetching.
const PER_STREAM_BYTES = 64 * 1024 * 1024 * 1024;

// Restart dropped lanes, but fail fast when a lane never establishes at all.
const LANE_RESTART_BACKOFF_MS = 300;
const LANE_MAX_RESTARTS = 40;
const EARLY_FAIL_RESTARTS = 3;
// A hung upload-session request should skip the stage, not ride into max-stall.
const UPLOAD_SESSION_TIMEOUT_MS = 3000;
// Stagger lanes so their TCP slow-start/loss cycles do not line up perfectly.
const LANE_STAGGER_MS = 75;

/** What `GET {path}/probe` proves about one role's path. Widens the generated
 *  `Probe` shape's protocol field, which an InfraInfo carried over from an
 *  earlier probe also has to fit. */
interface PathEvidence {
  clientIp: string;
  clientIpVersion: 4 | 6;
  clientIpSource: "socket" | "forwarded";
  protocolNegotiated: string;
}

// Some transports are advertised by the protocol before this client can drive them.
const RUNNABLE_TRANSPORT: Record<TransportKind, boolean> = {
  "fetch-stream": true,
  websocket: true,
  webtransport: false,
};

/** One worker pool + its bookkeeping for a single active transfer direction.
 *  A standalone download/upload stage populates exactly one of these; a
 *  bidirectional stage populates both ("down" and "up"), each with its own
 *  lane count, aggregation cadence, and stall/retry tracking, so one
 *  direction's health never skews the other's rate or restart backoff. */
interface ClientByteAggregation {
  pendingLaneBytes: number[];
  pendingLaneElapsedSec: number[];
  timer: ReturnType<typeof setInterval> | null;
  dbgWinBytes: number;
  dbgLastLog: number;
  lastAggregateAt: number;
}

interface LaneStreamState {
  dir: FlowDirection;
  /** The PhaseActivity.stage that owns this lane (e.g. "download" or
   *  "bidirectional") — the target for failStage, since a bidirectional
   *  down/up lane failure must report against "bidirectional", never the
   *  direction-derived "download"/"upload" name. */
  stage: PhaseActivity["stage"];
  /** Lanes for this direction, resolved from the stream policy at prime time and
   *  cached here because the lane-restart path (#spawnWorker via
   *  #onWorkerError) has no `activity` in scope. */
  laneCount: number;
  /** Experimental chunked-download mode (config flag, download only); the
   *  download worker self-sizes its `&bytes=N` requests when set. */
  chunkDownload: boolean;
  /** Per-lane spawn delay — LANE_STAGGER_MS, but shrunk so even the last lane
   *  spawns within the warmup window (0 ⇒ no warmup ⇒ spawn immediately). */
  laneStaggerMs: number;
  /** One worker per parallel stream, indexed by stream number. Download
   *  workers read-and-count; upload workers generate-and-stream. */
  workers: (Worker | null)[];
  /** The fetch URL each stream worker (re)starts against, by index. */
  streamUrls: string[];
  /** Per-lane byte windows waiting for the next aggregate sample. Each lane
   *  reports its own receive elapsed time, so the pool rate can be summed
   *  from matching per-lane numerators/denominators instead of a UI timer
   *  tick. Present only for client-counted download: server-counted upload has
   *  no local byte clock (the /upload/progress channel is its sole source). */
  clientAggregation: ClientByteAggregation | null;
  /** Monotonic measurement epoch (download only). Warmup batches carry seq=0;
   *  late messages from an old epoch are ignored at the warmup/measure
   *  boundary. Unused for "up" (upload has no client-side measure epoch). */
  measureSeq: number;
  /** True between onStageMeasure and onStageEnd for this direction — gates
   *  pushing samples. */
  measuring: boolean;
  /** True while THIS direction is stalled (independent of the other lane —
   *  see #setLaneStalled for how the two combine into the stage-level flag). */
  stalled: boolean;
  /** True once this direction has moved at least one byte — gates the
   *  never-established early fail (see EARLY_FAIL_RESTARTS). */
  stageSawBytes: boolean;
  /** Per-lane consecutive restart counter (reset on recovery) + backoff timers. */
  laneRetry: number[];
  laneTimers: (ReturnType<typeof setTimeout> | null)[];
}

export class RealBackend implements RunnerBackend {
  #opts: RealBackendOptions;
  /** The core handle: push samples / emit / report failures + health through it. */
  #host: CoreHost | null = null;
  /** AbortController for in-flight fetches/streams; aborted in onAbort. */
  #abort: AbortController | null = null;
  /** The transport established for the active phase, for stall/fail reporting. */
  #activeTransport: TransportKind | null = null;
  /** Independent role bindings are frozen from probe until the next run. */
  #throughputTarget: FetchThroughputTarget | null = null;
  #latencyTarget: WebSocketLatencyTarget | null = null;
  #streamPolicy: TransferStreamPolicy = { mode: "auto", count: 1 };
  #discoveryOrigin = "";
  #discoveryProtocol: string | undefined;
  #probeInfo: InfraInfo | null = null;

  /* ---- transfer stage state ----
   *  Bidirectional primes BOTH directions on the SAME stage (onStageBegin calls
   *  #primeTransfer once per activity.transfer entry), so this is keyed by
   *  FlowDirection rather than singular fields — a standalone download/upload
   *  stage just happens to populate exactly one entry. */
  /** One worker pool + its bookkeeping, per active transfer direction. */
  #lanes: Partial<Record<FlowDirection, LaneStreamState>> = {};
  /** True from the first #primeTransfer of the stage to #teardownTransfer —
   *  gates lane restarts so a late worker error after teardown can't respawn a
   *  lane. Shared across directions: both are primed and torn down together. */
  #transferActive = false;
  /** The STAGE-level stalled flag reported to the host (dedup so stall/resume
   *  each fire once per edge). For bidirectional this is the AND of both lanes
   *  — one direction hiccuping while the other still moves bytes must not
   *  surface a stage-wide stall (see #setLaneStalled). For a
   *  single-direction stage there's only one lane, so this is equivalent to
   *  that lane's own stalled state — no behavior change there. Also latches
   *  the idle-latency-only stall path (#latency's stall/resume) when no
   *  transfer is active at all (a stage with no byte lanes, e.g. "latency"). */
  #stalled = false;
  /** Per-run cache-buster seed, so `?cb=` is unique across runs and streams. */
  #cbSeed = "";

  /** The server-authoritative upload meter (up stage only). */
  #uploadProgress = new UploadProgressChannel({
    host: () => this.#host!,
    target: () => this.#throughputTarget,
    headers: () => this.#authHeaders(),
    lane: () => this.#lanes.up,
    transferActive: () => this.#transferActive,
    discardTransfer: () => this.#discardTransfer(),
    setLaneStalled: (stalled, detail) =>
      this.#setLaneStalled("up", stalled, detail),
  });

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
          transport: "websocket",
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
   *  browser can park the tab. A run overrides it — starting one is a
   *  deliberate foreground act. */
  #background = true;

  constructor(opts: RealBackendOptions = {}) {
    this.#opts = opts;
  }

  attach(host: CoreHost): void {
    this.#host = host;
  }

  #authHeaders(): HeadersInit | undefined {
    return this.#opts.authToken
      ? { authorization: `Bearer ${this.#opts.authToken}` }
      : undefined;
  }

  /* ================= PROBE ================= */
  /**
   * TARGET: same-origin `GET /preflight`.
   * Resolve `InfraInfo` (per-role client address, server identity, negotiated
   * protocols, engine version, pre-test ping). MAY `GET/WS {path}/ping` a few
   * times and emit pre-test `latency` samples (underLoad:false, negative `t`)
   * via host.emit for the sparkline. On failure, throw — engine.svelte.ts maps a
   * probe rejection to a `preflight-failed` error.
   * Cross-cutting: CORS + Timing-Allow-Origin for accurate timing.
   */
  async probe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo> {
    const previous = this.#probeInfo;
    if (role !== "throughput") this.#idle.stop();
    this.#throughputTarget = null;
    this.#latencyTarget = null;
    this.#discoveryProtocol = undefined;
    let pf: Preflight;
    try {
      // A logical server may restart with different public targets while the
      // SPA remains open, so every run resolves a fresh discovery document.
      const ident = `?client=web&client_version=${encodeURIComponent(BUILD.clientVersion)}`;
      const res = await authenticatedFetch(`/preflight${ident}`, {
        method: "GET",
        cache: "no-store",
        headers: this.#authHeaders(),
        signal,
      });
      if (!res.ok) throw new Error(`preflight returned HTTP ${res.status}`);
      pf = (await res.json()) as Preflight;
      this.#discoveryOrigin = new URL(res.url, location.href).origin;
      this.#discoveryProtocol = (
        performance.getEntriesByName(res.url, "resource").at(-1) as
          PerformanceResourceTiming | undefined
      )?.nextHopProtocol;
    } catch (cause) {
      await classifyAuthenticationFailure(signal);
      throw new Error(`preflight request failed: ${String(cause)}`, { cause });
    }
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
    if (previous?.discoveryGeneration !== pf.generation) role = undefined;
    const selection = config.transports.throughputTarget;
    const advertisedTarget = selectThroughputTarget(discovery, selection);
    if (!advertisedTarget)
      throw new TransportUnavailableError(`${selection} target unavailable`, {
        role: "throughput",
      });
    const selected = { ...advertisedTarget };
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
    this.#latencyTarget = selectLatencyTarget(
      discovery,
      config.transports.latencyTarget,
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

    const attempts = selected.protocol === "http3" ? 3 : 1;
    const deadline = performance.now() + 2000;
    let pathProbe: PathEvidence | null =
      role === "latency" && previous
        ? {
            clientIp: previous.clientIp,
            clientIpVersion: previous.clientIpVersion,
            clientIpSource: previous.clientIpSource,
            protocolNegotiated: previous.protocolNegotiated,
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
          ? setTimeout(() => probeCtl.abort(), 2000)
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
            headers: this.#authHeaders(),
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
      if (
        !pathProbe ||
        !browserProtocolMatchesTarget(selected, firstHopProtocol)
      )
        throw new TransportUnavailableError(
          `${selected.protocol} transport unavailable`,
          { role: "throughput" },
        );
      if (selected.protocol === "negotiated") {
        selected.protocol =
          protocolFromNextHop(firstHopProtocol) ?? "negotiated";
      }
    }
    if (!pathProbe)
      throw new TransportUnavailableError("throughput evidence unavailable", {
        role: "throughput",
      });

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
          headers: this.#authHeaders(),
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

    // Start the persistent idle keepalive (briskly at first) and use its first
    // few RTTs as the pre-test ping median (the server sends 0 — RTT is
    // client-measured). Best-effort: a ping failure must never fail preflight.
    if (needsLatency && role !== "throughput")
      await this.#idle.verifyReady(signal);
    const probeRtts =
      needsLatency && role !== "throughput"
        ? await this.#idle.collectRtts(signal)
        : [];

    const info: InfraInfo = {
      clientIp: pathProbe.clientIp,
      clientIpVersion: pathProbe.clientIpVersion,
      clientIpSource: pathProbe.clientIpSource,
      latencyClientIp: latencyPathProbe?.clientIp,
      latencyClientIpVersion: latencyPathProbe?.clientIpVersion,
      latencyClientIpSource: latencyPathProbe?.clientIpSource,
      server: {
        name: pf.server.name,
        location: pf.server.location,
      },
      preTestPingMs: probeRtts.length
        ? median(probeRtts)
        : (previous?.preTestPingMs ?? 0),
      engineVersion: pf.engineVersion,
      discoveryGeneration: pf.generation,
      protocolNegotiated: pathProbe.protocolNegotiated,
      selectedThroughputTarget: selected.id,
      selectedThroughputProtocol: selected.protocol,
      selectedLatencyTarget: this.#latencyTarget?.id,
      selectedLatencyTransport: this.#latencyTarget?.transport,
      latencyProtocolNegotiated: latencyPathProbe?.protocolNegotiated,
      firstHopProtocol,
      firstHopSecure: selected.tls,
    };
    this.#probeInfo = info;
    return info;
  }

  /** What THIS engine can drive today: WebSocket pings + fetch-stream transfer.
   *  Grows as webtransport/h3 land; a future per-role selection UI reads it. */
  describe(): EngineInfo {
    return {
      name: "real",
      version: BUILD.clientVersion, // built with the client; pluggable later
      latencyTransports: ["websocket"],
      throughputTransports: ["fetch-streams"],
    };
  }

  /* ================= LIFECYCLE (core → backend) ================= */
  /**
   * A run is starting. Open a fresh AbortController so onAbort can cancel
   * everything; reset any per-run state. Do NOT open transfer connections yet —
   * that happens per stage in onStageBegin.
   */
  onRunStart(config: RunnerConfig): void {
    // Pause the idle keepalive — the stage-owned ping channel takes over
    // latency duties for the duration of the run (resumed in #closeAll on
    // completion/abort).
    this.#idle.stop();
    this.#streamPolicy = { ...config.transferStreams };
    this.#abort = new AbortController();
    this.#activeTransport = null;
    this.#discardTransfer(); // discard leftovers from a prior run
    this.#stalled = false;
    // Unique-per-run cache-buster. performance.now() avoids Date.now and is
    // monotonic; the stream index is appended per worker.
    this.#cbSeed = `r${Math.round(performance.now())}`;
  }

  onStageBegin(activity: PhaseActivity): void | Promise<void> {
    const preparations: Promise<void>[] = [];
    // The ping channel is ALWAYS a latency-role transport (websocket today) — it
    // runs on its OWN socket, never on the stage's transfer transport. Negotiate
    // it separately (and first) so a loaded transfer stage's fetch-stream kind can
    // never reach the latency channel (which only services websocket), and so
    // #activeTransport ends as the transfer kind for the lanes' stall reporting.
    if (needsPings(activity)) {
      const pingKind = this.#negotiateTransport("latency");
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
      const kind = this.#negotiateTransport(activity.stage);
      if (!kind) {
        this.#host!.failStage(
          activity.stage,
          "transport-unavailable",
          "server offers no supported transfer transport",
        );
        return;
      }
      for (const dir of activity.transfer) {
        const preparation = this.#primeTransfer(kind, dir, activity);
        if (preparation) preparations.push(preparation);
      }
    }
    if (preparations.length > 0)
      return Promise.all(preparations).then(() => undefined);
  }

  /**
   * The stage's warmup window has elapsed; the connections primed in
   * onStageBegin are warm. Begin pushing real samples on the SAME connections —
   * NEVER reopen them (that would discard the warmup). Fires immediately after
   * onStageBegin when the stage has no warmup window (warmupMs<=0).
   *   • transfer lanes → #measureTransfer(dir): push host.ingestThroughput(dir,…).
   *   • ping channel    → #latency.measure(underLoad): push host.ingestLatency(…),
   *                       with underLoad = the stage moves bytes (bufferbloat).
   */
  onStageMeasure(activity: PhaseActivity): void {
    const underLoad = activity.transfer.length > 0;
    for (const dir of activity.transfer) this.#measureTransfer(dir);
    if (needsPings(activity)) this.#latency.measure(underLoad);
  }

  /** A measured stage ended. Drain its authoritative boundary sample before
   *  the core reduces the result, then release its connections. */
  onStageEnd(_activity: PhaseActivity, flush = true): void | Promise<void> {
    if (!flush) {
      this.#discardTransfer();
      this.#latency.teardown();
      return;
    }
    return this.#teardownTransfer().then(() => {
      this.#latency.teardown();
      this.#stalled = false;
    });
  }

  /** The run finished normally. Close anything still open. */
  onComplete(): void {
    this.#closeAll();
  }

  /** The user aborted. Cancel in-flight fetches/streams and close sockets. The
   *  core flips to "aborted" and emits the transition — do not emit here. */
  onAbort(): void {
    this.#closeAll();
  }

  /* ================= TRANSPORT NEGOTIATION ================= */
  /** Try transports in role order, reporting every attempt to the UI. A null
   *  result means the caller should skip/fail that stage based on its role. */
  #negotiateTransport(role: TransportRole): TransportKind | null {
    const host = this.#host!;
    for (const kind of this.#transportOrder(role)) {
      host.reportTransport({ kind, role, status: "negotiating" });
      const unavailable = this.#transportUnavailableReason(kind, role);
      if (!unavailable) {
        host.reportTransport({ kind, role, status: "established" });
        this.#activeTransport = kind;
        return kind;
      }
      host.reportTransport({
        kind,
        role,
        status: "failed",
        detail: unavailable,
      });
    }
    return null;
  }

  #transportUnavailableReason(
    kind: TransportKind,
    role: TransportRole,
  ): string | null {
    if (!RUNNABLE_TRANSPORT[kind]) return "not supported by this client";
    if (!this.#throughputTarget) return "no selected transfer target";
    let advertised: boolean;
    switch (kind) {
      case "fetch-stream":
        advertised = this.#throughputTarget.transport === "fetch-stream";
        break;
      case "websocket":
        advertised =
          role === "latency" && this.#latencyTarget?.transport === "websocket";
        break;
      case "webtransport":
        advertised = false;
        break;
    }
    return advertised ? null : "not advertised by server";
  }

  /** WebTransport stays first for future support; today it falls through to the
   *  serviced fallback: WebSocket for pings, fetch streams for byte lanes. */
  #transportOrder(role: TransportRole): TransportKind[] {
    return role === "latency"
      ? ["webtransport", "websocket"]
      : ["webtransport", "fetch-stream"];
  }

  /* ================= PRIME (warmup window) — open, don't measure ================= */
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
    });
  }

  /** Open the resolved transfer stream(s) for `dir` over `kind`
   *  (`GET {path}/download?bytes=N` for "down", `POST {path}/upload` streamed body
   *  for "up", or webtransport) and run priming bytes to warm the path (TCP
   *  congestion window / BBR / TLS) — pushing NOTHING into the core. The stream(s)
   *  stay open for #measureTransfer to start measuring on the SAME connection. */
  #primeTransfer(
    kind: TransportKind,
    dir: FlowDirection,
    activity: PhaseActivity,
  ): void | Promise<void> {
    if (kind !== "fetch-stream") throw new Error(`unsupported ${kind}`);

    // A stage names each direction once (bidirectional calls this twice, one
    // per direction) — a duplicate call for the SAME direction is a real bug.
    if (this.#lanes[dir]) throw new Error(`duplicate ${dir} prime`);

    const cfg = this.#host!.config!;
    const base = this.#throughputTarget!.origin;
    const laneCount = this.#streamCount(activity, dir);
    // Bound the stagger so the last lane (index laneCount−1) still spawns within
    // half the warmup; 0 when there's no warmup (lanes spawn together rather than
    // bleeding into the measured window).
    const staggerMs = laneStaggerMs(
      laneCount,
      cfg.duration.warmupMs,
      LANE_STAGGER_MS,
    );
    // Experimental: the download worker requests adaptive chunks itself, so omit the
    // baked-in ?bytes= and let it append &bytes=N per fetch (see download-worker.ts).
    const chunkDownload = dir === "down" && cfg.experimentalChunkedDownload;

    const state: LaneStreamState = {
      dir,
      stage: activity.stage,
      laneCount,
      chunkDownload,
      laneStaggerMs: staggerMs,
      workers: [],
      streamUrls: [],
      clientAggregation:
        dir === "down"
          ? {
              pendingLaneBytes: [],
              pendingLaneElapsedSec: [],
              timer: null,
              dbgWinBytes: 0,
              dbgLastLog: 0,
              lastAggregateAt: 0,
            }
          : null,
      measureSeq: 0,
      measuring: false,
      stalled: false,
      stageSawBytes: false,
      laneRetry: [],
      laneTimers: [],
    };
    this.#lanes[dir] = state;
    this.#transferActive = true;

    // Download streams the body down (?bytes=N to size it); upload streams a
    // generated body up (no size — the worker generates until the stage stops).
    // Upload gets its per-stage id asynchronously below before opening lanes.
    const url = (i: number, uploadId?: string): string => {
      const cb = `${this.#cbSeed}-${i}`;
      if (dir === "down") {
        const path = this.#throughputTarget?.routes.download ?? ROUTES.download;
        return chunkDownload
          ? `${base}${path}?cb=${cb}`
          : `${base}${path}?bytes=${PER_STREAM_BYTES}&cb=${cb}`;
      }
      const path = this.#throughputTarget?.routes.upload ?? ROUTES.upload;
      const idParam = uploadId ? `&id=${encodeURIComponent(uploadId)}` : "";
      return `${base}${path}?cb=${cb}${idParam}`;
    };

    if (dir === "up")
      return this.#primeUploadTransfer(dir, base, laneCount, url);

    for (let i = 0; i < laneCount; i++) {
      state.streamUrls[i] = url(i);
      this.#spawnLaneStaggered(dir, i);
    }
    // Workers start now (warming TCP cwnd). Download worker progress is tagged
    // seq=0 during warmup and ignored; #measureTransfer opens a new epoch and
    // resets the worker-side batch so no warmup bytes bleed into measurement.
    // Upload workers report no bytes (only `alive`) and the server count accrues
    // instead. Either way #measureTransfer starts the measurement path.
  }

  /** The lane is re-read after every await: a teardown or abort in flight
   *  clears `#lanes`, and continuing against a released lane would spawn
   *  workers no one owns. */
  async #primeUploadTransfer(
    dir: FlowDirection,
    base: string,
    laneCount: number,
    url: (i: number, uploadId?: string) => string,
  ): Promise<void> {
    const primedLane = this.#lanes[dir];
    let id: string;
    try {
      id = await this.#mintUploadSession(base);
    } catch {
      if (!this.#transferActive || !this.#lanes[dir]) return; // aborted/teardown while the warmup request was in flight
      this.#host!.failStage(
        primedLane!.stage,
        "protocol-error",
        "upload session request failed",
      );
      return;
    }
    const sessionLane = this.#lanes[dir];
    if (!this.#transferActive || !sessionLane) return;
    // The progress stream is the authoritative upload meter. Establish it
    // before any POST workers so forced H1 lanes cannot occupy every browser
    // connection slot and queue the control channel behind the upload.
    if (!(await this.#uploadProgress.prime(sessionLane.stage, id))) return;
    const progressLane = this.#lanes[dir];
    if (!this.#transferActive || !progressLane) return;
    for (let i = 0; i < laneCount; i++) {
      progressLane.streamUrls[i] = url(i, id);
      this.#spawnLaneStaggered(dir, i);
    }
  }

  async #mintUploadSession(base: string): Promise<string> {
    const path =
      this.#throughputTarget?.routes.uploadSession ?? ROUTES.uploadSession;
    // Own deadline + the run's abort: fetch must reject within the timeout even
    // when the request hangs, so the stage skips instead of max-stalling.
    const ctl = new AbortController();
    const onRunAbort = (): void => ctl.abort();
    this.#abort?.signal.addEventListener("abort", onRunAbort, { once: true });
    const deadline = setTimeout(() => ctl.abort(), UPLOAD_SESSION_TIMEOUT_MS);
    try {
      const res = await authenticatedFetch(`${base}${path}`, {
        method: "POST",
        cache: "no-store",
        signal: ctl.signal,
        headers: this.#authHeaders(),
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

  /** Spawn lane `i` at prime time, staggered by LANE_STAGGER_MS per index so the
   *  lanes don't slow-start in lockstep. Lane 0 is immediate; later lanes fire from
   *  #laneTimers[i] (which #teardownTransfer clears) within the warmup window. A
   *  lane can't be stagger-pending and restart-pending at once, so sharing the slot
   *  is safe. The URL is already stored before this runs. */
  #spawnLaneStaggered(dir: FlowDirection, i: number): void {
    const state = this.#lanes[dir]!;
    const delay = i * state.laneStaggerMs;
    if (delay <= 0) {
      this.#spawnWorker(dir, i);
      return;
    }
    state.laneTimers[i] = setTimeout(() => {
      state.laneTimers[i] = null;
      if (this.#transferActive) this.#spawnWorker(dir, i);
    }, delay);
  }

  /** Open (or re-open) the worker for stream `i` of direction `dir` against its
   *  stored URL. The worker script is chosen by direction; both speak the same
   *  start/stop ⇄ progress/error protocol. */
  #spawnWorker(dir: FlowDirection, i: number): void {
    const state = this.#lanes[dir];
    if (!state) return; // torn down before a staggered/restart timer fired
    const w = dir === "down" ? downloadWorker() : uploadWorker();
    w.onmessage = (e: MessageEvent) => this.#onWorkerMessage(dir, e.data, i);
    w.onerror = (e: ErrorEvent) =>
      this.#onWorkerError(dir, i, e.message || "worker error");
    // `debug`/`id` only drive the worker's own verbose per-stream logging.
    w.postMessage({
      type: "start",
      url: state.streamUrls[i],
      debug: debugEnabled(),
      id: i,
      streams: state.laneCount,
      credentials: authEnabled ? "include" : "same-origin",
      headers: {
        ...(this.#authHeaders() as Record<string, string> | undefined),
        // CSRF applies to the upload POST only. Adding it to the download GET
        // makes a cross-port transfer CORS-preflighted, and chunked mode
        // varies the URL per request, so the preflight cache never hits and
        // every measurement request pays a round trip it does not need.
        ...(dir === "up" ? csrfHeader() : {}),
      },
      // Download-only experimental chunked mode (ignored by the upload worker).
      chunk: state.chunkDownload,
    });
    if (state.measuring && dir === "down") {
      w.postMessage({ type: "measure", seq: state.measureSeq });
    }
    state.workers[i] = w;
  }

  /* ================= MEASURE — push real samples on the primed connections ====== */
  /** Begin measuring the already-open transfer stream(s) for `dir` (opened in
   *  #primeTransfer — NEVER reopen). Download sums its lanes in
   *  #aggregateDownload every THROUGHPUT_CADENCE_MS; upload is pushed by the
   *  server-authoritative progress channel instead. */
  #measureTransfer(dir: FlowDirection): void {
    const state = this.#lanes[dir];
    if (!state) return; // priming failed (failStage) — nothing to measure
    // Reuse the SAME workers primed during warmup — never re-spawn (that throws
    // away the warmed congestion window). Just open the measurement window:
    // discard whatever accrued during warmup and start aggregating.
    state.measuring = true;
    const aggregation = state.clientAggregation;
    if (aggregation) {
      aggregation.pendingLaneBytes = Array(state.laneCount).fill(0);
      aggregation.pendingLaneElapsedSec = Array(state.laneCount).fill(0);
    }
    if (dir === "down") {
      state.measureSeq++;
      for (const w of state.workers)
        w?.postMessage({ type: "measure", seq: state.measureSeq });
    } else {
      this.#uploadProgress.beginMeasure();
    }
    if (aggregation) {
      aggregation.dbgWinBytes = 0;
      aggregation.dbgLastLog = aggregation.lastAggregateAt = performance.now();
      aggregation.timer = setInterval(
        () => this.#aggregateDownload(state, aggregation),
        THROUGHPUT_CADENCE_MS,
      );
    }
  }

  /** Aggregation tick: sum the byte deltas `dir`'s workers reported since the
   *  last tick into one real sample tagged with that direction. For download,
   *  each lane contributes bytes / that lane's own receive interval, then the
   *  lane rates are summed. Exact bytes and cadence time feed the final reducer,
   *  including zero-byte windows. Upload never enters this path: its server
   *  progress stream owns both bytes and time, so a local timer would double its
   *  denominator and inject false zero-rate samples into the UI. */
  #aggregateDownload(
    state: LaneStreamState,
    aggregation: ClientByteAggregation,
  ): void {
    const now = performance.now();
    const durationSec = (now - aggregation.lastAggregateAt) / 1000;
    aggregation.lastAggregateAt = now;
    let delta = 0;
    let bytesPerSec = 0;
    for (let i = 0; i < aggregation.pendingLaneBytes.length; i++) {
      const laneBytes = aggregation.pendingLaneBytes[i] ?? 0;
      const laneSec = aggregation.pendingLaneElapsedSec[i] ?? 0;
      aggregation.pendingLaneBytes[i] = 0;
      aggregation.pendingLaneElapsedSec[i] = 0;
      if (laneBytes <= 0 || laneSec <= 0) continue;
      delta += laneBytes;
      bytesPerSec += laneBytes / laneSec;
    }
    if (durationSec > 0)
      this.#host!.ingestThroughput("down", bytesPerSec, delta, durationSec);
    // Verbose: the pool's combined raw rate, 1 Hz. This is the sum the core
    // then smooths (see core:throughput) — comparing this to the per-worker
    // raw logs shows whether aggregation loses anything, and to the server
    // figure whether bytes are lost between the wire and JS.
    if (debugEnabled()) {
      aggregation.dbgWinBytes += delta;
      const dt = now - aggregation.dbgLastLog;
      if (dt >= 1000) {
        const active = state.workers.reduce((n, w) => n + (w ? 1 : 0), 0);
        dlog("realrunner:aggregate", "down pool", {
          rate: fmtRate(aggregation.dbgWinBytes / (dt / 1000)),
          tick: fmtRate(bytesPerSec),
          window: fmtBytes(aggregation.dbgWinBytes),
          streams: active,
          dt: fmtMs(dt),
        });
        aggregation.dbgWinBytes = 0;
        aggregation.dbgLastLog = now;
      }
    }
  }

  /** A transfer worker reported liveness, bytes, or a stream error.
   *   • download `progress` → client-counted bytes drive the live curve: accrue
   *     into the aggregation window and clear any open stall.
   *   • upload `alive` → one POST completed; reset the lane's restart counter. The
   *     server /upload/progress count is the SOLE upload byte source, so an upload lane
   *     reports NO bytes and never drives the curve or resumes a stall here (the
   *     progress worker owns the up curve + its stall/resume — see #uploadProgress).
   *   • `error` → stall + restart that single lane (#onWorkerError). */
  #onWorkerMessage(
    dir: FlowDirection,
    msg:
      | { type: "progress"; bytes: number; elapsedMs?: number; seq?: number }
      | { type: "alive" }
      | { type: "error"; recoverable: boolean; detail: string }
      | AuthRequiredMsg,
    i: number,
  ): void {
    const state = this.#lanes[dir];
    if (!state) return; // late message after teardown
    if (msg.type === "auth-required") {
      this.#discardTransfer();
      redirectToLogin();
      return;
    }
    if (msg.type === "progress") {
      if (
        dir === "down" &&
        ("seq" in msg ? msg.seq !== state.measureSeq || msg.seq <= 0 : true)
      ) {
        return;
      }
      state.laneRetry[i] = 0; // a real send proves this lane recovered
      state.stageSawBytes = true;
      const elapsedMs = msg.elapsedMs ?? THROUGHPUT_CADENCE_MS;
      const aggregation = state.clientAggregation;
      if (!aggregation) return;
      aggregation.pendingLaneBytes[i] =
        (aggregation.pendingLaneBytes[i] ?? 0) + msg.bytes;
      aggregation.pendingLaneElapsedSec[i] =
        (aggregation.pendingLaneElapsedSec[i] ?? 0) + elapsedMs / 1000;
      if (state.stalled) this.#setLaneStalled(dir, false);
    } else if (msg.type === "alive") {
      state.laneRetry[i] = 0; // a completed POST proves this upload lane recovered
      state.stageSawBytes = true;
    } else {
      this.#onWorkerError(dir, i, msg.detail, msg.recoverable);
    }
  }

  /** Handle a transfer lane failure (download or upload). Recoverable (the common
   *  case: a dropped connection) → stall once, then re-open the lane so a real
   *  sample resumes it. Only call fail() when the drop is genuinely unrecoverable. */
  #onWorkerError(
    dir: FlowDirection,
    i: number,
    detail: string,
    recoverable = true,
  ): void {
    // Ignore late errors after teardown (a stop()/terminate races the worker).
    if (!this.#transferActive) return;
    const state = this.#lanes[dir];
    if (!state) return;
    if (!recoverable) {
      this.#host!.fail(
        "connection-lost",
        `${dir} stream ${i} failed: ${detail}`,
        detail,
      );
      return;
    }
    if (state.measuring) this.#setLaneStalled(dir, true, detail);
    // Tear the lane down now; re-open it after a backoff so a persistently-
    // failing stream can't spin a tight respawn loop (re-creating workers
    // hundreds of times/sec). Give up the run once a lane
    // exhausts its restarts — the core's max-stall timeout also bounds patience.
    state.workers[i]?.terminate();
    state.workers[i] = null;
    state.laneRetry[i] = (state.laneRetry[i] ?? 0) + 1;
    // Never moved a byte this stage + repeated instant failures ⇒ the endpoint
    // is unreachable/unsupported. Skip the stage instead of stalling for 20 s.
    if (!state.stageSawBytes && state.laneRetry[i] > EARLY_FAIL_RESTARTS) {
      this.#host!.failStage(
        state.stage,
        "connection-lost",
        `${dir} link connection could not be established`,
      );
      return;
    }
    if (state.laneRetry[i] > LANE_MAX_RESTARTS) {
      this.#host!.fail(
        "connection-lost",
        `${dir} stream ${i} kept dropping: ${detail}`,
        detail,
      );
      return;
    }
    state.laneTimers[i] = setTimeout(() => {
      state.laneTimers[i] = null;
      if (this.#transferActive) this.#spawnWorker(dir, i);
    }, LANE_RESTART_BACKOFF_MS);
  }

  /** Reconcile a direction's own stall state into the STAGE-level `#stalled`
   *  flag reported to the host. Bidirectional's two directions are combined
   *  with AND: the stage is stalled only once EVERY currently-primed direction
   *  is — one lane hiccuping while the other still moves bytes must not surface
   *  a stage-wide warning. A single-direction stage has exactly
   *  one entry in `#lanes`, so this reduces to that lane's own stalled state —
   *  no behavior change there. */
  #setLaneStalled(dir: FlowDirection, stalled: boolean, detail?: string): void {
    const state = this.#lanes[dir];
    if (!state || state.stalled === stalled) return;
    state.stalled = stalled;
    const allStalled = Object.values(this.#lanes).every((s) => s!.stalled);
    if (allStalled && !this.#stalled) {
      this.#host!.stall({
        reason: "connection-lost",
        transport: this.#activeTransport ?? undefined,
        detail,
      });
      this.#stalled = true;
    } else if (!allStalled && this.#stalled) {
      this.#host!.resume();
      this.#stalled = false;
    }
  }

  #stopTransferWorkers(): void {
    // Preserve the partial download cadence window at the stage boundary.
    const down = this.#lanes.down;
    if (down?.measuring && down.clientAggregation)
      this.#aggregateDownload(down, down.clientAggregation);
    for (const state of Object.values(this.#lanes)) {
      if (!state) continue;
      const timer = state.clientAggregation?.timer;
      if (timer != null) clearInterval(timer);
      for (const t of state.laneTimers) if (t) clearTimeout(t);
      for (const w of state.workers) if (w) stopWorker(w);
    }
  }

  /** Stop every POST lane, wait for the server's terminal upload count, then
   *  release the stage state. */
  async #teardownTransfer(): Promise<void> {
    this.#stopTransferWorkers();
    // Stop the progress worker AFTER the POST lanes — BYE must follow the lanes
    // ending so the server's final count includes everything they drained.
    await this.#uploadProgress.teardown(true);
    this.#transferActive = false;
    this.#lanes = {};
  }

  #discardTransfer(): void {
    this.#stopTransferWorkers();
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
    // The run (or abort) just ended — resume the idle keepalive so the
    // connectivity pill stays live again instead of freezing at its
    // last-known state until the next probe/run.
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
