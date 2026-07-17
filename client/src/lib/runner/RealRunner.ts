// Real measurement backend: negotiates browser transports, owns workers, and
// pushes only measured wire samples into RunnerCore.
import type {
  RunnerConfig,
  PingCadence,
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
  Preflight,
  WebSocketLatencyTarget,
} from "../api/preflight";
import type { Probe } from "../api/probe";
import { debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../debug";
import { BUILD } from "../buildenv";
import { transferStreamCount } from "./real/streamPolicy";
import {
  httpToWs,
  median,
  needsPings,
  laneStaggerMs,
  selectThroughputTarget,
  selectLatencyTarget,
  browserProtocolMatchesTarget,
  classifyTransportDiscovery,
  throughputTargetKey,
} from "./real/backendPure";

export interface RealBackendOptions {
  authToken?: string;
}

export class TransportUnavailableError extends Error {}

// Match the core/dummy cadence so both engines feed the UI at the same rate.
const THROUGHPUT_CADENCE_MS = 60;

// Large enough that a normal stage ends by aborting the stream, not by refetching.
const PER_STREAM_BYTES = 64 * 1024 * 1024 * 1024;

// Restart dropped lanes, but fail fast when a lane never establishes at all.
const LANE_RESTART_BACKOFF_MS = 300;
const LANE_MAX_RESTARTS = 40;
const EARLY_FAIL_RESTARTS = 3;
const PING_ESTABLISH_TIMEOUT_MS = 3500;
// A hung upload-session request should skip the stage, not ride into max-stall.
const UPLOAD_SESSION_TIMEOUT_MS = 3000;
const PROGRESS_ESTABLISH_TIMEOUT_MS = 3500;
// Stagger lanes so their TCP slow-start/loss cycles do not line up perfectly.
const LANE_STAGGER_MS = 75;

const PROGRESS_BYE_GRACE_MS = 1000;

// Ping pacing is separate for idle, latency, and loaded-transfer contexts.
const PING_INTERVAL: Record<PingCadence, number> = {
  instant: 80,
  medium: 250,
  slow: 600,
};
const PING_MAX_IN_FLIGHT = 16;
const PING_LOADED_MAX_IN_FLIGHT = 2;
const PING_REPORT_GAP_MS = 20;
const PING_LOSS_K = 4;
const PING_LOSS_FLOOR_MS = 250;

// One low-rate idle ping worker powers connectivity and preflight RTT outside runs.
const IDLE_PING_INTERVAL_MS = 1000;
const PROBE_PING_INTERVAL_MS = 120;
const PROBE_PING_COUNT = 5;
const PROBE_PING_TIMEOUT_MS = 1500;
const IDLE_RESPAWN_MS = 2000;

// Some transports are advertised by the protocol before this client can drive them.
const RUNNABLE_TRANSPORT: Record<TransportKind, boolean> = {
  "fetch-stream": true,
  websocket: true,
  webtransport: false,
};
/** One measured ping the worker reports (rtt already computed in-worker). */
interface PingSample {
  rtt: number;
  lost: boolean;
}
/** Ping worker → RealRunner messages. The worker owns reconnection, so it emits
 *  stall/resume around a reconnect window rather than a terminal error. */
type PingOutMsg =
  | { type: "open" }
  | { type: "samples"; samples: PingSample[] }
  | { type: "stall"; detail: string }
  | { type: "resume" };

/** Upload-progress worker → RealRunner. `bytes`/`complete` carry the SERVER's
 *  cumulative drained count `n` and elapsed clock `t` (ns) it was
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
   *  tick. Populated by download progress messages only — an upload lane
   *  reports no bytes (the server /upload/progress channel is the sole source). */
  /** Present only for client-counted download. Server-counted upload has no
   * local byte clock by construction. */
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
  /** Server capabilities from the last successful probe (advertised origins +
   *  endpoint paths + which transports are available). Stashed here so later
   *  stages negotiate transports against what the server actually offers. */
  #capabilities: Preflight["capabilities"] | null = null;
  /** Independent role bindings are frozen from probe until the next run. */
  #throughputTarget: FetchThroughputTarget | null = null;
  #latencyTarget: WebSocketLatencyTarget | null = null;
  #streamPolicy: TransferStreamPolicy = { mode: "auto", count: 1 };
  #discoveryOrigin = "";
  #discoveryProtocol: string | undefined;
  #probeInfo: InfraInfo | null = null;

  /* ---- transfer stage state (Stage 2 download, Stage 3 upload, Stage 6 bidi) ----
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
   *  the idle-latency-only stall path (#onPingMessage) when no transfer is
   *  active at all (a stage with no byte lanes, e.g. plain "latency"). */
  #stalled = false;
  /** Per-run cache-buster seed, so `?cb=` is unique across runs and streams. */
  #cbSeed = "";

  /* ---- server-authoritative upload state (Stage 3+) ---- */
  /** The upload-session id minted during upload warmup; appended as &id= on the
   *  upload POST lanes AND the /upload/progress stream so the server correlates them.
   *  null ⇒ the current upload stage has not been allocated yet. */
  #testId: string | null = null;
  /** The dedicated /upload/progress progress worker (up stage only), or null. */
  #progressWorker: Worker | null = null;
  #progressReady: { finish: (ready: boolean) => void } | null = null;
  #progressDone: (() => void) | null = null;
  /** Latest cumulative server byte count and previous measured snapshot. */
  #srvN = 0;
  #srvPrevN = 0; // cumulative at the last delta fed into the live curve
  #srvPrevT = 0; // server elapsed ns of that last delta — the live-curve denominator
  #srvHaveStart = false;

  /* ---- latency (ping) stage state (Stage 4) ---- */
  /** The dedicated ping worker: owns the WebSocket bus and timestamps RTTs
   *  in-worker. One per stage (idle latency, then each loaded transfer stage). */
  #pingWorker: Worker | null = null;
  /** True from #primeLatencyChannel to #teardownLatency — gates late worker
   *  messages after teardown. */
  #pingActive = false;
  /** Armed for the idle latency stage only: fires failStage("latency") when no
   *  pong ever arrives. Cleared by the first sample / teardown. */
  #pingEstablishTimer: ReturnType<typeof setTimeout> | null = null;
  /** The underLoad tag stamped on forwarded samples (true during a transfer
   *  stage's loaded latency). Set when #measureLatency flips reporting on. */
  #latencyUnderLoad = false;

  /* ---- idle keepalive (connectivity indicator + preflight ping) ----
   * Separate from the stage-scoped #pingWorker/#pingActive above; never
   * active at the same time (stopped in onRunStart, restarted on run end). */
  #idleWorker: Worker | null = null;
  #idleActive = false;
  #idleTargetKey = "";
  /** Set while probe() is harvesting the keepalive's first RTTs; `finish`
   *  resolves the preflight median wait (idempotent). */
  #probeCollect: { rtts: number[]; finish: () => void } | null = null;
  /** True after the keepalive reported a stall, so "connected" is emitted only
   *  on the offline→online edge instead of once per sample. */
  #idleOffline = false;
  /** Pending respawn of a dead idle worker (script failed to load / crashed);
   *  see IDLE_RESPAWN_MS. Cleared on stop. */
  #idleRespawnTimer: ReturnType<typeof setTimeout> | null = null;

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

  #downloadWorker(): Worker {
    return new Worker(
      new URL("./workers/download-worker.ts", import.meta.url),
      {
        type: "module",
      },
    );
  }

  #uploadWorker(): Worker {
    return new Worker(new URL("./workers/upload-worker.ts", import.meta.url), {
      type: "module",
    });
  }

  #progressWorkerInstance(): Worker {
    return new Worker(
      new URL("./workers/upload-progress-worker.ts", import.meta.url),
      { type: "module" },
    );
  }

  #pingWorkerInstance(): Worker {
    return new Worker(new URL("./workers/ping-worker.ts", import.meta.url), {
      type: "module",
    });
  }

  #resetUploadCounters(): void {
    this.#srvN = 0;
    this.#srvPrevN = 0;
    this.#srvPrevT = 0;
    this.#srvHaveStart = false;
  }

  /* ================= PROBE ================= */
  /**
   * TARGET: same-origin `GET /preflight`.
   * Resolve `InfraInfo` (per-role client address, server identity, negotiated
   * protocols, engine version, pre-test ping). MAY `GET/WS {path}/ping` a few
   * times and emit pre-test `latency` samples (underLoad:false, negative `t`)
   * via host.emit for the sparkline. On failure, throw — wire.ts maps a probe
   * rejection to a `preflight-failed` error.
   * Cross-cutting: CORS + Timing-Allow-Origin for accurate timing.
   */
  async probe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo> {
    const previous = this.#probeInfo;
    if (role !== "throughput") this.#stopIdleKeepalive();
    this.#capabilities = null;
    this.#throughputTarget = null;
    this.#latencyTarget = null;
    this.#discoveryProtocol = undefined;
    let pf: Preflight;
    try {
      // A logical server may restart with different public targets while the
      // SPA remains open, so every run resolves a fresh discovery document.
      const ident = `?client=web&client_version=${encodeURIComponent(BUILD.clientVersion)}`;
      const res = await fetch(`/preflight${ident}`, {
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
      throw new Error(`preflight request failed: ${String(cause)}`, { cause });
    }
    this.#capabilities = pf.capabilities;
    const discovery = classifyTransportDiscovery(
      pf.capabilities.throughputTargets,
      pf.capabilities.latencyTargets,
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
    const selected = selectThroughputTarget(discovery, selection);
    if (!selected)
      throw new TransportUnavailableError(`${selection} target unavailable`);
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
    if (needsLatency && !this.#latencyTarget)
      throw new TransportUnavailableError(
        `${config.transports.latencyTarget} latency target unavailable`,
      );
    if (!needsLatency) this.#latencyTarget = null;

    const attempts = selected.protocol === "http3" ? 3 : 1;
    const deadline = performance.now() + 2000;
    let pathProbe: {
      clientIp: string;
      clientIpVersion: 4 | 6;
      clientIpSource: "socket" | "forwarded";
      protocolNegotiated: string;
    } | null =
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
          const probeRes = await fetch(probeURL, {
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
        if (selected.protocol === "http3")
          throw new TransportUnavailableError("http3 transport unavailable", {
            cause,
          });
        throw cause;
      } finally {
        if (probeDeadline !== undefined) clearTimeout(probeDeadline);
      }
      if (
        !pathProbe ||
        !browserProtocolMatchesTarget(selected, firstHopProtocol)
      )
        throw new TransportUnavailableError(
          `${selected.protocol} transport unavailable`,
        );
    }
    if (!pathProbe)
      throw new TransportUnavailableError("throughput evidence unavailable");

    let verifiedLatencyProtocol =
      role === "throughput" ? previous?.verifiedLatencyProtocol : undefined;
    let latencyPathProbe:
      | {
          clientIp: string;
          clientIpVersion: 4 | 6;
          clientIpSource: "socket" | "forwarded";
          protocolNegotiated: string;
        }
      | undefined =
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
      const latencyRes = await fetch(latencyURL, {
        cache: "no-store",
        headers: this.#authHeaders(),
        signal,
      });
      if (!latencyRes.ok)
        throw new TransportUnavailableError(
          `latency probe returned HTTP ${latencyRes.status}`,
        );
      latencyPathProbe = (await latencyRes.json()) as Probe;
      verifiedLatencyProtocol = (
        performance.getEntriesByName(latencyRes.url, "resource").at(-1) as
          PerformanceResourceTiming | undefined
      )?.nextHopProtocol;
      if (verifiedLatencyProtocol !== "http/1.1")
        throw new TransportUnavailableError(
          `latency target negotiated ${verifiedLatencyProtocol || "unknown"}, want http/1.1`,
        );
      if (latencyPathProbe.protocolNegotiated !== "http/1.1")
        throw new TransportUnavailableError(
          `latency server observed ${latencyPathProbe.protocolNegotiated}, want http/1.1`,
        );
    }

    // Start the persistent idle keepalive (briskly at first) and use its first
    // few RTTs as the pre-test ping median (the server sends 0 — RTT is
    // client-measured). Best-effort: a ping failure must never fail preflight.
    const probeRtts =
      needsLatency && role !== "throughput"
        ? await this.#collectIdleRtts(signal)
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
        host: pf.server.host,
        port: pf.server.port,
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
      verifiedLatencyProtocol,
      latencyProtocolNegotiated: latencyPathProbe?.protocolNegotiated,
      firstHopProtocol,
      firstHopSecure: selected.tls,
    };
    this.#probeInfo = info;
    return info;
  }

  /** Start the idle keepalive at the brisk probe cadence and resolve with its
   *  first PROBE_PING_COUNT RTTs (median → preTestPingMs), then settle the
   *  worker to the 1/s idle cadence. Best-effort: resolves with whatever it
   *  gathered by the timeout, never rejects. */
  #collectIdleRtts(signal?: AbortSignal): Promise<number[]> {
    if (signal?.aborted) return Promise.resolve([]);
    this.#startIdleKeepalive(PROBE_PING_INTERVAL_MS);
    if (!this.#idleWorker) return Promise.resolve([]);
    return new Promise<number[]>((resolve) => {
      const finish = (): void => {
        if (!this.#probeCollect) return;
        clearTimeout(timer);
        signal?.removeEventListener("abort", finish);
        const rtts = this.#probeCollect.rtts;
        this.#probeCollect = null;
        this.#idleWorker?.postMessage({
          type: "measure",
          intervalMs: IDLE_PING_INTERVAL_MS,
        });
        resolve(rtts);
      };
      const timer = setTimeout(finish, PROBE_PING_TIMEOUT_MS);
      signal?.addEventListener("abort", finish, { once: true });
      this.#probeCollect = { rtts: [], finish };
    });
  }

  /** The server capabilities captured by the last successful probe (or null
   *  before probing). Consumed by later-stage transport negotiation. */
  get capabilities(): Preflight["capabilities"] | null {
    return this.#capabilities;
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
    this.#stopIdleKeepalive();
    this.#streamPolicy = { ...config.transferStreams };
    this.#abort = new AbortController();
    this.#activeTransport = null;
    this.#discardTransfer(); // discard leftovers from a prior run
    this.#stalled = false;
    this.#testId = null;
    // Unique-per-run cache-buster. performance.now() avoids Date.now and is
    // monotonic; the stream index is appended per worker.
    this.#cbSeed = `r${Math.round(performance.now())}`;
  }

  onStageBegin(activity: PhaseActivity): void | Promise<void> {
    const preparations: Promise<void>[] = [];
    // The ping channel is ALWAYS a latency-role transport (websocket today) — it
    // runs on its OWN socket, never on the stage's transfer transport. Negotiate
    // it separately (and first) so a loaded transfer stage's fetch-stream kind can
    // never reach #primeLatencyChannel (which only services websocket), and so
    // #activeTransport ends as the transfer kind for the lanes' stall reporting.
    if (needsPings(activity)) {
      const pingKind = this.#negotiateTransport("latency");
      if (pingKind) {
        this.#primeLatencyChannel(pingKind, activity.stage === "latency");
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
   *   • ping channel    → #measureLatency(underLoad): push host.ingestLatency(…),
   *                       with underLoad = the stage moves bytes (bufferbloat).
   */
  onStageMeasure(activity: PhaseActivity): void {
    const underLoad = activity.transfer.length > 0;
    for (const dir of activity.transfer) this.#measureTransfer(dir);
    if (needsPings(activity)) this.#measureLatency(underLoad);
  }

  /** A measured stage ended. Drain its authoritative boundary sample before
   *  the core reduces the result, then release its connections. */
  onStageEnd(activity: PhaseActivity, flush = true): void | Promise<void> {
    void activity;
    if (!flush) {
      this.#discardTransfer();
      this.#teardownLatency();
      return;
    }
    return this.#teardownTransfer().then(() => {
      this.#teardownLatency();
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
    const streams = laneCount;
    // Bound the stagger so the last lane (index laneCount−1) still spawns within
    // half the warmup; 0 when there's no warmup (lanes spawn together rather than
    // bleeding into the measured window).
    const staggerMs = laneStaggerMs(
      streams,
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
        const path = this.#throughputTarget?.routes.download ?? "/download";
        return chunkDownload
          ? `${base}${path}?cb=${cb}`
          : `${base}${path}?bytes=${PER_STREAM_BYTES}&cb=${cb}`;
      }
      const path = this.#throughputTarget?.routes.upload ?? "/upload";
      const idParam = uploadId ? `&id=${encodeURIComponent(uploadId)}` : "";
      return `${base}${path}?cb=${cb}${idParam}`;
    };

    if (dir === "up") {
      this.#testId = null;
      return this.#primeUploadTransfer(dir, base, streams, url);
    }

    for (let i = 0; i < streams; i++) {
      state.streamUrls[i] = url(i);
      this.#spawnLaneStaggered(dir, i);
    }
    // Workers start now (warming TCP cwnd). Download worker progress is tagged
    // seq=0 during warmup and ignored; #measureTransfer opens a new epoch and
    // resets the worker-side batch so no warmup bytes bleed into measurement.
    // Upload workers report no bytes (only `alive`) and the server count accrues
    // instead. Either way #measureTransfer starts the measurement path.
  }

  async #primeUploadTransfer(
    dir: FlowDirection,
    base: string,
    streams: number,
    url: (i: number, uploadId?: string) => string,
  ): Promise<void> {
    const primed = this.#lanes[dir];
    let id: string;
    try {
      id = await this.#mintUploadSession(base);
    } catch (cause) {
      if (!this.#transferActive || !this.#lanes[dir]) return; // aborted/teardown while the warmup request was in flight
      void cause;
      this.#host!.failStage(
        primed!.stage,
        "protocol-error",
        "upload session request failed",
      );
      return;
    }
    const state = this.#lanes[dir];
    if (!this.#transferActive || !state) return;
    this.#testId = id;
    // The progress stream is the authoritative upload meter. Establish it
    // before any POST workers so forced H1 lanes cannot occupy every browser
    // connection slot and queue the control channel behind the upload.
    if (!(await this.#primeUploadProgress(state.stage))) return;
    const readyState = this.#lanes[dir];
    if (!this.#transferActive || !readyState) return;
    for (let i = 0; i < streams; i++) {
      readyState.streamUrls[i] = url(i, id);
      this.#spawnLaneStaggered(dir, i);
    }
  }

  async #mintUploadSession(base: string): Promise<string> {
    const path =
      this.#throughputTarget?.routes.uploadSession ?? "/upload/session";
    // Own deadline + the run's abort: fetch must reject within the timeout even
    // when the request hangs, so the stage skips instead of max-stalling.
    const ctl = new AbortController();
    const onRunAbort = (): void => ctl.abort();
    this.#abort?.signal.addEventListener("abort", onRunAbort, { once: true });
    const deadline = setTimeout(() => ctl.abort(), UPLOAD_SESSION_TIMEOUT_MS);
    try {
      const res = await fetch(`${base}${path}`, {
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
    } finally {
      clearTimeout(deadline);
      this.#abort?.signal.removeEventListener("abort", onRunAbort);
    }
  }

  /** Establish the server-authoritative upload progress stream before starting
   *  POST lanes. Upload cannot be measured honestly without this channel. */
  #primeUploadProgress(stage: PhaseActivity["stage"]): Promise<boolean> {
    this.#resetUploadCounters();

    if (!this.#testId) return Promise.resolve(false);
    const target = this.#throughputTarget;
    const progressRoute = target?.routes.uploadProgress;
    if (!target || !progressRoute) {
      this.#host!.failStage(
        stage,
        "transport-unavailable",
        "selected throughput target has no upload progress route",
      );
      return Promise.resolve(false);
    }

    const url = `${target.origin}${progressRoute}?id=${encodeURIComponent(this.#testId)}`;
    const w = this.#progressWorkerInstance();
    const ready = new Promise<boolean>((resolve) => {
      const finish = (established: boolean): void => {
        if (this.#progressReady?.finish !== finish) return;
        clearTimeout(timer);
        this.#progressReady = null;
        resolve(established);
      };
      const timer = setTimeout(() => {
        this.#host!.failStage(
          stage,
          "connection-lost",
          "upload progress channel could not be established",
        );
        finish(false);
      }, PROGRESS_ESTABLISH_TIMEOUT_MS);
      this.#progressReady = { finish };
    });
    w.onmessage = (e: MessageEvent<ProgressOutMsg>): void => {
      if (e.data.type === "open") this.#progressReady?.finish(true);
      this.#onProgressMessage(e.data);
    };
    w.onerror = (): void => {
      /* the worker owns reconnect; a hard worker error just means no server bytes
       * until it recovers, which the stall watchdog already covers. */
    };
    w.postMessage({
      type: "start",
      url,
      headers: this.#opts.authToken
        ? { authorization: `Bearer ${this.#opts.authToken}` }
        : undefined,
    });
    this.#progressWorker = w;
    return ready;
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
    const w = dir === "down" ? this.#downloadWorker() : this.#uploadWorker();
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
      // Download-only experimental chunked mode (ignored by the upload worker).
      chunk: state.chunkDownload,
    });
    if (state.measuring && dir === "down") {
      w.postMessage({ type: "measure", seq: state.measureSeq });
    }
    state.workers[i] = w;
  }

  /** Open the latency (ping) channel over `kind` and warm it. Spawns the
   *  dedicated ping worker (which owns the WebSocket + the whole ping algorithm),
   *  hands it the tuning, and lets it send warmup pings — pushing NOTHING into
   *  the core. #measureLatency flips reporting on over the SAME warmed socket. */
  #primeLatencyChannel(kind: TransportKind, isLatencyStage = false): void {
    if (kind !== "websocket") throw new Error(`unsupported ${kind}`);

    const cfg = this.#host!.config!;
    const channel = this.#latencyTarget;
    const latencyRoute = channel?.routes.ping;
    if (!channel || channel.transport !== "websocket" || !latencyRoute)
      throw new Error("latency target not resolved");
    const url = httpToWs(channel.origin) + latencyRoute;
    const cadence = isLatencyStage ? cfg.pingCadence : cfg.loadedPingCadence;
    const intervalMs = PING_INTERVAL[cadence];

    this.#latencyUnderLoad = false;
    this.#pingActive = true;
    // The idle latency stage has no byte lanes to prove the link — bound how
    // long the channel gets to deliver its first pong before the stage skips.
    if (isLatencyStage) {
      this.#pingEstablishTimer = setTimeout(() => {
        this.#pingEstablishTimer = null;
        this.#host!.failStage(
          "latency",
          "connection-lost",
          "ping connection could not be established",
        );
      }, PING_ESTABLISH_TIMEOUT_MS);
    }
    const w = this.#pingWorkerInstance();
    w.onmessage = (e: MessageEvent<PingOutMsg>): void =>
      this.#onPingMessage(e.data);
    w.onerror = (e: ErrorEvent): void =>
      this.#onPingMessage({
        type: "stall",
        detail: e.message || "ping worker error",
      });
    w.postMessage({
      type: "start",
      url,
      intervalMs,
      maxInFlight: isLatencyStage
        ? PING_MAX_IN_FLIGHT
        : PING_LOADED_MAX_IN_FLIGHT,
      reportGapMs: PING_REPORT_GAP_MS,
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
    });
    this.#pingWorker = w;
  }

  /** Handle a message from the ping worker. The worker reports already-computed
   *  RTTs; the runner just tags underLoad and forwards. stall/resume bracket a
   *  reconnect — surfaced to the core ONLY for the idle latency stage; during a
   *  transfer stage the byte lanes drive link health, so loaded-latency
   *  reconnects pass silently. */
  #onPingMessage(msg: PingOutMsg): void {
    if (!this.#pingActive) return; // late message after teardown
    switch (msg.type) {
      case "samples":
        this.#clearPingEstablishTimer(); // a pong proves the channel works
        for (const s of msg.samples) {
          this.#host!.ingestLatency(s.rtt, this.#latencyUnderLoad, s.lost);
        }
        break;
      case "stall":
        if (!this.#transferActive && !this.#stalled) {
          this.#host!.stall({
            reason: "connection-lost",
            transport: "websocket",
            detail: msg.detail,
          });
          this.#stalled = true;
        }
        break;
      case "resume":
        if (!this.#transferActive && this.#stalled) {
          this.#host!.resume();
          this.#stalled = false;
        }
        break;
      case "open":
        break;
    }
  }

  #clearPingEstablishTimer(): void {
    if (this.#pingEstablishTimer) {
      clearTimeout(this.#pingEstablishTimer);
      this.#pingEstablishTimer = null;
    }
  }

  /** Stop + terminate the ping worker (closes its WebSocket). Idempotent. */
  #teardownLatency(): void {
    this.#pingActive = false;
    this.#clearPingEstablishTimer();
    if (this.#pingWorker) {
      this.#pingWorker.postMessage({ type: "stop" });
      this.#pingWorker.terminate();
      this.#pingWorker = null;
    }
  }

  /* ================= IDLE KEEPALIVE (connectivity indicator) ================= */
  /** Start the persistent idle ping at `intervalMs` (default 1/s). Safe to
   *  call repeatedly — no-ops if already running or if websocket isn't
   *  available. Started by probe() (at the brisk preflight cadence) and again
   *  after every run ends (via #closeAll, from onComplete/onAbort), so the
   *  connectivity pill stays live whenever the app isn't mid-test. The
   *  uses a tiny in-flight window and a fixed internal cadence. */
  #startIdleKeepalive(intervalMs = IDLE_PING_INTERVAL_MS): void {
    const targetKey = `${throughputTargetKey(this.#throughputTarget)}\n${this.#latencyTarget?.id ?? ""}`;
    if (this.#idleActive && this.#idleTargetKey === targetKey) return;
    if (this.#idleActive) this.#stopIdleKeepalive();
    const channel = this.#latencyTarget;
    const latencyRoute = channel?.routes.ping;
    if (!channel || channel.transport !== "websocket" || !latencyRoute) return;
    const url = httpToWs(channel.origin) + latencyRoute;
    this.#idleActive = true;
    this.#idleTargetKey = targetKey;
    // Treat connectivity as unknown until this (fresh) worker proves the link:
    // its first samples then emit a "connected" edge. Crucial after a
    // connection-lost failure — the store latched the pulse offline, and
    // without this edge a link that recovered before the worker's first stall
    // would never un-latch it.
    this.#idleOffline = true;
    const w = this.#pingWorkerInstance();
    w.onmessage = (e: MessageEvent<PingOutMsg>): void =>
      this.#onIdlePingMessage(e.data);
    w.onerror = (e: ErrorEvent): void => {
      // Worker died without ever running its reconnect loop — most commonly the
      // script fetch itself failed because the (bundle-serving) server is down,
      // e.g. restarting the keepalive right after a connection-lost run. Report
      // offline and retry the SPAWN until one sticks (the in-worker reconnect
      // loop only exists once the script loads).
      this.#onIdlePingMessage({
        type: "stall",
        detail: e.message || "idle ping worker error",
      });
      this.#scheduleIdleRespawn(intervalMs);
    };
    w.postMessage({
      type: "start",
      url,
      intervalMs,
      maxInFlight: 2,
      reportGapMs: 0, // paced sends are already sparse — report every sample
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
    });
    // Report immediately (there is no keepalive warmup window).
    w.postMessage({ type: "measure" });
    this.#idleWorker = w;
  }

  /** Stop the idle keepalive — a real run is starting (onRunStart), or the
   *  app is tearing down. Idempotent. */
  #stopIdleKeepalive(): void {
    this.#idleActive = false;
    this.#idleTargetKey = "";
    if (this.#idleRespawnTimer) {
      clearTimeout(this.#idleRespawnTimer);
      this.#idleRespawnTimer = null;
    }
    this.#probeCollect?.finish();
    if (this.#idleWorker) {
      this.#idleWorker.postMessage({ type: "stop" });
      this.#idleWorker.terminate();
      this.#idleWorker = null;
    }
  }

  /** Re-spawn the idle worker after it died at load time (see the onerror
   *  handler in #startIdleKeepalive). One timer at a time; each failed attempt
   *  schedules the next, so the keepalive keeps knocking every IDLE_RESPAWN_MS
   *  until the server is back to serve the script. */
  #scheduleIdleRespawn(intervalMs?: number): void {
    if (!this.#idleActive || this.#idleRespawnTimer) return;
    this.#idleRespawnTimer = setTimeout(() => {
      this.#idleRespawnTimer = null;
      if (!this.#idleActive) return; // a run started (or teardown) meanwhile
      this.#stopIdleKeepalive();
      this.#startIdleKeepalive(intervalMs);
    }, IDLE_RESPAWN_MS);
  }

  /** Handle a message from the idle ping worker. Idle samples never reach
   *  `host.ingestLatency` (run accumulation) — they are emitted as raw
   *  `latency` events tagged phase "idle", which the store routes to its
   *  keepalive-only buffer; the `connectivity` event is the hard override
   *  effectiveConnectivity respects (stall()/resume() no-op outside a run). */
  #onIdlePingMessage(msg: PingOutMsg): void {
    if (!this.#idleActive) return;
    switch (msg.type) {
      case "samples":
        for (const s of msg.samples) {
          if (this.#probeCollect && !s.lost) {
            this.#probeCollect.rtts.push(s.rtt);
            if (this.#probeCollect.rtts.length >= PROBE_PING_COUNT)
              this.#probeCollect.finish();
          }
          this.#host!.emit({
            type: "latency",
            sample: {
              t: 0,
              rttMs: s.rtt,
              underLoad: false,
              lost: s.lost,
              phase: "idle",
            },
          });
        }
        if (this.#idleOffline) {
          this.#idleOffline = false;
          this.#host!.emit({ type: "connectivity", state: "connected" });
        }
        break;
      case "stall":
        this.#idleOffline = true;
        this.#host!.emit({ type: "connectivity", state: "offline" });
        break;
      case "resume":
      case "open":
        break;
    }
  }

  /* ================= MEASURE — push real samples on the primed connections ====== */
  /** Begin measuring the already-open transfer stream(s) for `dir` (opened in
   *  #primeTransfer — NEVER reopen). Per #readLoop, sum received/sent bytes/sec
   *  across streams and push host.ingestThroughput(dir, bytesPerSec, bytes) at
   *  ~16Hz. */
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
      // The first progress frame after this boundary becomes the upload baseline,
      // excluding warmup bytes and time together.
      this.#srvHaveStart = false;
      this.#srvPrevN = this.#srvN;
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
   *     progress worker owns the up curve + its stall/resume — see #onProgressMessage).
   *   • `error` → stall + restart that single lane (#onWorkerError). */
  #onWorkerMessage(
    dir: FlowDirection,
    msg:
      | { type: "progress"; bytes: number; elapsedMs?: number; seq?: number }
      | { type: "alive" }
      | { type: "error"; recoverable: boolean; detail: string },
    i: number,
  ): void {
    const state = this.#lanes[dir];
    if (!state) return; // late message after teardown
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
  #onProgressMessage(msg: ProgressOutMsg): void {
    const state = this.#lanes.up;
    if (!this.#transferActive || !state) return; // late message after teardown
    if (msg.type === "fatal") {
      this.#progressReady?.finish(false);
      if (state.measuring) {
        this.#host!.fail(
          "connection-lost",
          `upload progress failed: ${msg.detail}`,
          msg.detail,
        );
      } else {
        this.#host!.failStage(state.stage, "connection-lost", msg.detail);
      }
      return;
    }
    if (msg.type === "stall") {
      // The progress stream dropped: no server bytes until it reconnects. Freeze
      // surface recovery immediately instead of waiting for the silence watchdog.
      if (state.measuring) this.#setLaneStalled("up", true, msg.detail);
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
      state.stageSawBytes = true;
    }
    if (!state.measuring) return; // warmup bytes are excluded from the window

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
      this.#host!.ingestThroughput(
        "up",
        delta / frameSec,
        delta,
        frameSec,
        true,
      );
    }
    if (delta > 0) {
      this.#setLaneStalled("up", false);
    }
    if (msg.type === "complete") this.#progressDone?.();
  }

  /** Stop the progress worker after the POST lanes. It explicitly finalizes the
   *  session with DELETE and lets the stream receive the terminal complete record. */
  #teardownProgress(finalize: boolean): Promise<void> {
    this.#progressReady?.finish(false);
    this.#progressReady = null;
    const w = this.#progressWorker;
    this.#progressWorker = null;
    if (!w) return Promise.resolve();
    const worker = w;
    if (!finalize) {
      worker.terminate();
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      const timer = setTimeout(done, PROGRESS_BYE_GRACE_MS);
      const self = this;
      function done(): void {
        clearTimeout(timer);
        if (self.#progressDone === done) self.#progressDone = null;
        worker.terminate();
        resolve();
      }
      this.#progressDone = done;
      worker.postMessage({ type: "stop" });
    });
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
      for (const w of state.workers) {
        if (!w) continue;
        w.postMessage({ type: "stop" });
        w.terminate();
      }
    }
  }

  /** Stop every POST lane, wait for the server's terminal upload count, then
   *  release the stage state. */
  async #teardownTransfer(): Promise<void> {
    this.#stopTransferWorkers();
    // Stop the progress worker AFTER the POST lanes — BYE must follow the lanes
    // ending so the server's final count includes everything they drained.
    await this.#teardownProgress(true);
    this.#transferActive = false;
    this.#lanes = {};
  }

  #discardTransfer(): void {
    this.#stopTransferWorkers();
    void this.#teardownProgress(false);
    this.#transferActive = false;
    this.#lanes = {};
  }

  /** Begin measuring on the already-open ping channel (opened in
   *  #primeLatencyChannel). RTT = now − sent; an unacked / timed-out ping is
   *  `lost`. The channel retains the cadence selected when its stage warmup
   *  began; measurement only enables reporting via
   *  host.ingestLatency(rtt, underLoad, lost) — `underLoad` is true when the
   *  pings run concurrently with a transfer (bufferbloat). */
  #measureLatency(underLoad: boolean): void {
    // The worker primed in #primeLatencyChannel is already pinging on a warm
    // socket; just flip reporting on (never re-spawn — that would throw away the
    // warmup). underLoad tags every forwarded sample: true when these pings run
    // concurrently with a transfer (bufferbloat), false for the idle stage.
    this.#latencyUnderLoad = underLoad;
    this.#pingWorker?.postMessage({ type: "measure" });
  }

  #closeAll(): void {
    this.#discardTransfer();
    this.#teardownLatency();
    this.#stalled = false;
    if (this.#abort) {
      this.#abort.abort();
      this.#abort = null;
    }
    this.#activeTransport = null;
    // The run (or abort) just ended — resume the idle keepalive so the
    // connectivity pill stays live again instead of freezing at its
    // last-known state until the next probe/run.
    this.#startIdleKeepalive();
  }
}
