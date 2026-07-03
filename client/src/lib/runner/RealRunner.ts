/* ============================================================
 * The Graphite Meter — Real Backend SKELETON (§14.4 / §transport)
 * ============================================================
 *
 * A compile-clean stub implementing `RunnerBackend` (core.ts) so a
 * future engine talking to live speedtest-server APIs can be filled
 * in WITHOUT touching the core, the UI, or the store. The shared
 * `RunnerCore` already owns the timeline, sequencing, the MEASURED
 * test-time clock + stall accrual, accumulation, stability,
 * early-stop, result reduction, and the whole event stream — so a
 * real engine implements ONLY network I/O and link-health signals.
 *
 * ── What the backend is responsible for ─────────────────────
 *  • probe()         — the preflight handshake (+ a few pre-test pings).
 *  • transport       — negotiate webtransport / websocket / xhr-stream per
 *                    phase; report each attempt with host.reportTransport; if
 *                    every kind fails, host.fail("transport-unavailable", …).
 *  • connection lifecycle — a STAGE owns its connection(s) across its whole
 *                    warmup→measure→end span: open + PRIME on onStageBegin, START
 *                    measuring the SAME connections on onStageMeasure, close on
 *                    onStageEnd / onAbort / onComplete. The warmup genuinely
 *                    warms the wire the measurement runs over — never reopen at
 *                    the seam. The CORE decides WHEN stages change; the backend
 *                    just reacts. Every hook carries the stage's PhaseActivity
 *                    (transfer lanes + loadedLatency), so the backend reads
 *                    NOTHING from global config to know what to open.
 *  • measurement     — stream/measure raw throughput + latency and push them
 *                    in via host.ingestThroughput(dir, …) / host.ingestLatency.
 *                    Direction travels WITH the sample, so a bidirectional
 *                    phase pushes both "down" and "up". (Push model: do NOT
 *                    implement onTick — that is the dummy's pull hook. Real
 *                    samples arrive as bytes do.)
 *  • link health     — on a mid-phase drop call host.stall({reason, transport,
 *                    detail}) (NON-terminal: the core freezes measured-time so
 *                    the phase end recedes by the dead-air); on reconnect call
 *                    host.resume(). A real sample arriving also auto-resumes via
 *                    the core watchdog. Only an UNRECOVERABLE drop is a terminal
 *                    host.fail(...).
 *  • failures        — report unrecoverable drops/timeouts/protocol errors with
 *                    host.fail(reason, message, cause). User abort is the
 *                    core's abort()/"aborted" phase, NOT a failure.
 *
 * ── Real-stats-only (principle 1) ───────────────────────────
 *  Push ONLY real measured samples. NEVER synthesize a sample to fill dead air,
 *  decay a value to zero on a stall, or zero-fill a gap — the ~800ms
 *  grind-to-zero is a render-layer effect (principle 2), computed by the UI from
 *  store.stalledSince. The backend stores/emits nothing during a stall.
 *
 * ── To go live ──────────────────────────────────────────────
 *  Change ONE line in wire.svelte.ts:
 *      runner = new RunnerCore(new RealBackend({ endpoint: ... }));
 *  Nothing else in the app changes.
 *
 * ── Units rule (§14.0 / §14.4) ──────────────────────────────
 *  Raw on the wire: the server serves/sinks BYTES; the client derives
 *  bytes/sec. Push raw instantaneous bytes/sec + the bytes moved over the
 *  interval into host.ingestThroughput. NO bits / base-2/10 / unit
 *  conversion in the backend — that is the UI's job.
 * ============================================================ */

import type {
  RunnerConfig,
  RunnerAnomaly,
  InfraInfo,
  EngineInfo,
  ServerCandidate,
  TransportKind,
  TransportRole,
  FlowDirection,
  PhaseActivity,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import type { Preflight } from "../api/preflight";
import { debugEnabled, dlog, fmtRate, fmtBytes, fmtMs } from "../debug";
import { BUILD } from "../buildenv";

/** Resolve the fetch base URL for the backend. `host:"auto"` (or empty) means
 *  same-origin (relative requests) — the Stage-1 case where the Go server serves
 *  both the app and the API. A concrete host builds an absolute origin. */
function resolveBase(endpoint?: RunnerConfig["endpoint"]): string {
  if (!endpoint || endpoint.host === "auto" || endpoint.host === "") return "";
  const scheme = endpoint.port === 443 ? "https" : "http";
  return `${scheme}://${endpoint.host}:${endpoint.port}`;
}

/** Map an http(s) origin to its ws(s) equivalent for the latency bus. Anything
 *  already ws(s):// (or relative) passes through unchanged. */
function httpToWs(origin: string): string {
  if (origin.startsWith("https://")) return "wss://" + origin.slice("https://".length);
  if (origin.startsWith("http://")) return "ws://" + origin.slice("http://".length);
  return origin;
}

/** Construction options for a real engine: the endpoint to target plus anything
 *  preflight hands back (e.g. a session token). All optional so the class is
 *  trivial to drop into wire.svelte.ts. */
export interface RealBackendOptions {
  /** Backend base the API paths in §14.4 are relative to. Falls back to the
   *  `config.endpoint` passed into start()/probe() when omitted. */
  endpoint?: RunnerConfig["endpoint"];
  /** Optional bearer/session token from a prior preflight (§14.4 auth). */
  authToken?: string;
}

const NOT_IMPL = (method: string) =>
  new Error(`RealBackend.${method} not implemented`);

/** Throughput push cadence: aggregate worker deltas and push ~16 Hz (mirrors
 *  the dummy's THROUGHPUT_CADENCE_MS so both engines feel identical). */
const THROUGHPUT_CADENCE_MS = 60;

/** Bytes requested per download stream. Sized so ONE request outlasts any
 *  reasonable stage even on a fast link (64 GiB — the server's clamp ceiling —
 *  lasts ~60 s/stream at 1 GB/s), so a stage never churns connections
 *  mid-measurement; the worker still re-fetches if a stream genuinely ends
 *  early, and we abort at stage end. */
const PER_STREAM_BYTES = 64 * 1024 * 1024 * 1024; // 64 GiB

/** Backoff before re-opening a dropped lane, so a persistently-failing stream
 *  can't spin a tight respawn loop (re-creating workers + re-fetching wasm). */
const LANE_RESTART_BACKOFF_MS = 300;
/** Give up a lane after this many consecutive restarts (≈12 s at the backoff);
 *  the core's max-stall timeout also bounds total patience. */
const LANE_MAX_RESTARTS = 40;
/** If a stage has NEVER moved a byte, give up much sooner: a lane failing this
 *  many times in a row means the connection can't be established at all, so the
 *  stage is skipped (failStage) instead of stalling toward max-stall. */
const EARLY_FAIL_RESTARTS = 3;
/** How long the ping channel gets to deliver its first pong before the latency
 *  stage is skipped as unreachable. */
const PING_ESTABLISH_TIMEOUT_MS = 3500;
/** Deadline for the upload-session mint. Without it a hung request (e.g. the
 *  connection pool still draining the download lanes) neither resolves nor
 *  rejects, and the upload stage rides silently into the 20 s max-stall. */
const UPLOAD_SESSION_TIMEOUT_MS = 3000;
/** Per-lane spawn delay so the lanes' TCP slow-starts don't ramp in lockstep
 *  (synchronised overshoot → synchronised loss → synchronised backoff). At ≤4
 *  staggered lanes this is ≤300 ms, comfortably inside the warmup window. */
const LANE_STAGGER_MS = 75;

/** The browser's ~6-connections-per-origin limit — the pool the lane budget is
 *  carved from (see #laneBudget). */
const BROWSER_CONN_BUDGET = 6;
/** Connections kept free of POST lanes: ONLY the buses this phase needs
 *  (/ws/ping, /ws/upload) each take one. No extra reserve is held — the data
 *  lanes are keep-alive and reuse their connection on respawn, and the preflight
 *  OPTIONS completes before the transfer phase opens, so neither contends with a
 *  bus frame during measurement. Reserving a phantom slot here would silently
 *  drop every phase one lane below the requested budget. */
/** Grace after BYE for the /ws/upload worker to flush + receive UPLOAD_COMPLETE
 *  before we terminate it (the client headline is already set, so we don't block). */
const PROGRESS_BYE_GRACE_MS = 1000;

/** Median of a non-empty number list (used for the pre-test ping). */
function median(xs: number[]): number {
  const sorted = [...xs].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/* ---------- Latency (ping) tuning, handed to the ping worker ---------- */
/** Pacer interval per pingConcurrency knob — the floor send rate that keeps
 *  multiple pings on the wire on high-RTT links (mirrors the dummy's map). */
const PING_INTERVAL: Record<RunnerConfig["pingConcurrency"], number> = {
  instant: 80,
  medium: 250,
  slow: 600,
};
/** Max concurrent in-flight pings — bounds wire spam and the worker's pending
 *  map. Low-RTT links rarely exceed 1–2; high-RTT links fill toward this. */
const PING_MAX_IN_FLIGHT = 16;
/** Loaded-phase cadence: under a transfer the idle chain would spray hundreds of
 *  tiny PINGs/sec upstream and starve the download's ACKs on an asymmetric line.
 *  A loaded-latency distribution needs only a few samples/sec, so we kill the
 *  on-receive chain and pace a 2-deep window at ~8 Hz — enough to characterize
 *  bufferbloat, negligible uplink load. */
const PING_LOADED_INTERVAL_MS = 120;
const PING_LOADED_MAX_IN_FLIGHT = 2;
/** Min gap between sends, so the sub-ms on-receive chain tops out at ~1 kHz. */
const PING_MIN_GAP_MS = 1;
/** Min gap between UI-bound samples (worker downsamples to this). Decouples the
 *  report rate from the ping rate: on a fast link the chain pings ~1 kHz, but the
 *  main thread sees ≤ ~50 samples/s — comparable to the 16 Hz throughput path —
 *  so host.ingestLatency isn't flooded. The loss estimator still sees every pong;
 *  on slow links (pings farther apart than this) nothing is dropped. */
const PING_REPORT_GAP_MS = 20;
/** RTTVAR multiplier for the loss timeout (RFC 6298-style RTO = SRTT + K·RTTVAR).
 *  The deviation term spikes on an abrupt RTT jump, so the timeout adapts UP
 *  within ~1 RTT instead of false-flagging loss. */
const PING_LOSS_K = 4;
/** Loss-timeout floor (ms) — a sanity minimum that governs cold start before the
 *  estimator has a sample; the adaptive term takes over after the first pong. */
const PING_LOSS_FLOOR_MS = 250;

/* ---------- Idle keepalive (connectivity indicator + preflight ping) ----------
 * ONE persistent, low-rate ping worker covers everything outside a run: it
 * starts during preflight at a brisk burst cadence (to fill the sparkline and
 * derive the pre-test ping median quickly), then settles to 1 ping/s. It is
 * stopped for the duration of an actual test (the stage-owned ping channel
 * takes over latency duties then) and restarted the instant it ends. Its
 * samples are tagged phase "idle" and NEVER enter run buffers (the store
 * routes them to its own small ring). /ws/ping is a stateless echo, so
 * holding it open indefinitely costs nothing extra server-side. */
const IDLE_PING_INTERVAL_MS = 1000;
/** Preflight burst: cadence + how many samples the pre-test median wants. */
const PROBE_PING_INTERVAL_MS = 120;
const PROBE_PING_COUNT = 5;
/** Hard cap on the probe burst, so a slow/dead bus never delays preflight. */
const PROBE_PING_TIMEOUT_MS = 1500;
/** Respawn cadence for a dead idle worker. The worker's OWN reconnect loop
 *  handles a dropped socket, but it can't run if the worker script never
 *  loaded — the same server that echoes pings also serves the bundle, so
 *  restarting the keepalive while the server is down kills the Worker at
 *  fetch time (`onerror`, no reconnect loop). Retry the spawn itself until
 *  one sticks. Matches the worker's own RECONNECT_MAX_MS. */
const IDLE_RESPAWN_MS = 2000;

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
 *  cumulative drained count `n` AND the server monotonic clock `t` (ns) it was
 *  sampled at — the SOLE upload byte source. Rate is derived over server time
 *  (Δn / Δt), so the live curve and the totals headline are both immune to local
 *  tick/arrival jitter. stall/resume bracket a reconnect; since there is no
 *  client-side fallback, they are forwarded to the core to freeze measured-time
 *  across the gap (see #onProgressMessage). */
type ProgressOutMsg =
  | { type: "open" }
  | { type: "bytes"; n: number; t: number }
  | { type: "complete"; n: number; t: number }
  | { type: "stall"; detail: string }
  | { type: "resume" };

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

  /* ---- transfer stage state (Stage 2 download, Stage 3 upload) ---- */
  /** The active transfer direction for the current stage's worker pool. */
  #dir: FlowDirection = "down";
  /** Lanes for the active stage, derived per-phase by #laneBudget at prime time
   *  and cached here because the lane-restart path (#spawnWorker via #onWorkerError)
   *  has no `activity` in scope. Also the per-worker upload reservoir split. */
  #laneCount = 1;
  /** Experimental chunked-download mode for the active stage (config flag); the
   *  download worker self-sizes its `&bytes=N` requests when set. */
  #chunkDownload = false;
  /** Per-lane spawn delay for this stage — LANE_STAGGER_MS, but shrunk so even the
   *  last lane spawns within the warmup window (0 ⇒ no warmup ⇒ spawn immediately). */
  #laneStaggerMs = 0;
  /** One worker per parallel stream, indexed by stream number. Download workers
   *  read-and-count; upload workers generate-and-stream. Same message protocol. */
  #workers: (Worker | null)[] = [];
  /** The fetch URL each stream worker (re)starts against, by index. */
  #streamUrls: string[] = [];
  /** Per-download-lane byte windows waiting for the next aggregate sample. Each
   *  lane reports its own receive elapsed time, so the pool rate can be summed
   *  from matching per-lane numerators/denominators instead of a UI timer tick. */
  #pendingLaneBytes: number[] = [];
  #pendingLaneElapsedSec: number[] = [];
  /** Monotonic download measurement epoch. Warmup batches carry seq=0; late
   *  messages from an old epoch are ignored at the warmup/measure boundary. */
  #downloadMeasureSeq = 0;
  /** The ~16 Hz aggregation timer; null while not measuring. */
  #aggTimer: ReturnType<typeof setInterval> | null = null;
  /** True from #primeTransfer to #teardownTransfer — gates lane restarts so a
   *  late worker error after teardown can't respawn a lane. */
  #transferActive = false;
  /** Per-lane consecutive restart counter (reset on recovery) + backoff timers. */
  #laneRetry: number[] = [];
  #laneTimers: (ReturnType<typeof setTimeout> | null)[] = [];
  /** True between onStageMeasure and onStageEnd — gates pushing samples. */
  #measuring = false;
  /** True while a stall is open (so a recovering sample resumes exactly once). */
  #stalled = false;
  /** True once the active transfer stage has moved at least one byte — gates
   *  the never-established early fail (see EARLY_FAIL_RESTARTS). */
  #stageSawBytes = false;
  /** Per-run cache-buster seed, so `?cb=` is unique across runs and streams. */
  #cbSeed = "";

  /* ---- server-authoritative upload state (Stage 3+) ---- */
  /** The upload-session id minted during upload warmup; appended as &id= on the
   *  upload POST lanes AND the /ws/upload socket so the server correlates them.
   *  null ⇒ the current upload stage has not been allocated yet. */
  #testId: string | null = null;
  /** The dedicated /ws/upload progress worker (up stage only), or null. */
  #progressWorker: Worker | null = null;
  /** Latest cumulative server byte count + the measured-window anchors for the
   *  totals-based headline = (lastN − startN) / (lastT − startT), where T is the
   *  server's ACTIVE measurement clock (ns bytes were flowing, dead zones excluded),
   *  NOT a wall span across frames — so a stall/reconnect/early-finish can't dilute
   *  the denominator with idle time. */
  #srvN = 0;
  #srvPrevN = 0; // cumulative at the last delta fed into the live curve
  #srvPrevT = 0; // server ACTIVE-time (ns) of that last delta — the live-curve denominator
  #srvStartN = 0;
  #srvStartT = 0; // server ACTIVE-time (ns) at the first measured frame — the headline window anchor
  #srvHaveStart = false;
  /** Verbose 1 Hz aggregate-log window: bytes summed across the pool since the
   *  last log + its start time. Lets the pool's combined raw rate be compared
   *  against the per-worker raw logs and the server `-verbose` figure. */
  #dbgWinBytes = 0;
  #dbgLastLog = 0;

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

  /** Resolve the backend base: prefer the endpoint passed in, else construction. */
  #resolveEndpoint(
    passed?: RunnerConfig["endpoint"],
  ): RunnerConfig["endpoint"] | undefined {
    return passed ?? this.#opts.endpoint;
  }

  /* ================= PROBE ================= */
  /**
   * TARGET: `GET {base}/preflight` (a.k.a. /config) — §14.4.
   * Resolve `InfraInfo` (client public IP, server identity, negotiated
   * protocol, engine version, pre-test ping). MAY `GET/WS {path}/ping` a few
   * times and emit pre-test `latency` samples (underLoad:false, negative `t`)
   * via host.emit for the sparkline. On failure, throw — wire.ts maps a probe
   * rejection to a `preflight-failed` error.
   * Cross-cutting: CORS + Timing-Allow-Origin for accurate timing.
   */
  async probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo> {
    const base = resolveBase(this.#resolveEndpoint(endpoint));
    let res: Response;
    try {
      // Identify the client to the server (version negotiation seam): a future
      // server can key feature/compat decisions off these. Query params (not a
      // custom header) so a cross-origin preflight GET stays a simple request.
      const ident = `?client=web&client_version=${encodeURIComponent(BUILD.clientVersion)}`;
      res = await fetch(`${base}/preflight${ident}`, {
        method: "GET",
        headers: this.#opts.authToken
          ? { authorization: `Bearer ${this.#opts.authToken}` }
          : undefined,
      });
    } catch (cause) {
      // Network-level failure (server down, DNS, CORS). wire.ts maps the
      // rejection to a `preflight-failed` error.
      throw new Error(`preflight request failed: ${String(cause)}`, { cause });
    }
    if (!res.ok) {
      throw new Error(`preflight returned HTTP ${res.status}`);
    }
    const pf = (await res.json()) as Preflight;
    this.#capabilities = pf.capabilities;

    // Start the persistent idle keepalive (briskly at first) and use its first
    // few RTTs as the pre-test ping median (the server sends 0 — RTT is
    // client-measured). Best-effort: a ping failure must never fail preflight.
    const probeRtts = await this.#collectIdleRtts(endpoint);

    return {
      clientIp: pf.clientIp,
      server: {
        name: pf.server.name,
        host: pf.server.host,
        port: pf.server.port,
        location: pf.server.location,
      },
      preTestPingMs: probeRtts.length ? median(probeRtts) : pf.preTestPingMs,
      engineVersion: pf.engineVersion,
      protocolNegotiated: pf.protocolNegotiated,
    };
  }

  /** Start the idle keepalive at the brisk probe cadence and resolve with its
   *  first PROBE_PING_COUNT RTTs (median → preTestPingMs), then settle the
   *  worker to the 1/s idle cadence. Best-effort: resolves with whatever it
   *  gathered by the timeout, never rejects. */
  #collectIdleRtts(endpoint: RunnerConfig["endpoint"]): Promise<number[]> {
    this.#startIdleKeepalive(endpoint, PROBE_PING_INTERVAL_MS);
    if (!this.#idleWorker) return Promise.resolve([]);
    return new Promise<number[]>((resolve) => {
      const finish = (): void => {
        if (!this.#probeCollect) return;
        clearTimeout(timer);
        const rtts = this.#probeCollect.rtts;
        this.#probeCollect = null;
        this.#idleWorker?.postMessage({ type: "measure", intervalMs: IDLE_PING_INTERVAL_MS });
        resolve(rtts);
      };
      const timer = setTimeout(finish, PROBE_PING_TIMEOUT_MS);
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
    void this.#resolveEndpoint(config.endpoint);
    this.#abort = new AbortController();
    this.#activeTransport = null;
    this.#teardownTransfer(); // clear any leftover lanes from a prior run
    this.#measuring = false;
    this.#stalled = false;
    this.#pendingLaneBytes = [];
    this.#pendingLaneElapsedSec = [];
    this.#downloadMeasureSeq = 0;
    this.#testId = null;
    // Unique-per-run cache-buster. performance.now() avoids Date.now and is
    // monotonic; the stream index is appended per worker.
    this.#cbSeed = `r${Math.round(performance.now())}`;
  }

  /**
   * A stage is beginning — the start of its warmup window (or the stage itself
   * when warmupMs<=0). Negotiate the transport ONCE for the stage, then open and
   * PRIME every connection `activity` names — WITHOUT measuring yet:
   *   • activity.transfer — the byte lanes ("down"/"up"); [] for the latency stage.
   *   • a ping channel    — when activity.loadedLatency, or the latency stage.
   * The same connections are reused in onStageMeasure, so the warmup genuinely
   * warms the wire the measurement runs over (no cold reconnect at the seam).
   * The stub bodies below throw NOT_IMPL — fill them in to go live.
   */
  onStageBegin(activity: PhaseActivity): void {
    // The ping channel is ALWAYS a latency-role transport (websocket today) — it
    // runs on its OWN socket, never on the stage's transfer transport. Negotiate
    // it separately (and first) so a loaded transfer stage's xhr-stream kind can
    // never reach #primeLatencyChannel (which only services websocket), and so
    // #activeTransport ends as the transfer kind for the lanes' stall reporting.
    if (this.#needsPings(activity)) {
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
      for (const dir of activity.transfer) this.#primeTransfer(kind, dir, activity);
    }
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
    if (this.#needsPings(activity)) this.#measureLatency(underLoad);
  }

  /** A stage's measured window has ended (boundary, early finish, or run end).
   *  Close/cancel the stage's connection(s); the core has already finalized its
   *  result. */
  onStageEnd(activity: PhaseActivity): void {
    void activity;
    // The core has finalized this stage's result; release its connections. For
    // download that means stopping + terminating the worker pool; for latency (or
    // a transfer stage's loaded-latency pings) the ping worker + its socket.
    this.#teardownTransfer();
    this.#teardownLatency();
    this.#stalled = false; // a stale latch must not swallow the next stage's stall
  }

  /** Whether a stage runs a ping channel: the idle latency stage, or a transfer
   *  stage with loaded latency (bufferbloat) active. */
  #needsPings(activity: PhaseActivity): boolean {
    return (
      activity.stage === "latency" || (activity.transfer.length > 0 && activity.loadedLatency)
    );
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

  /* ================= TRANSPORT NEGOTIATION (§transport) ================= */
  /**
   * Negotiate a transport for `role`, trying the configured kinds in preference
   * order (webtransport first, then the config's websocket/xhr-stream fallback).
   * Report EACH step with host.reportTransport({kind, role, status}):
   *   negotiating → attempting this kind;
   *   established → connected — return the kind so the caller opens its I/O;
   *   failed      → this kind didn't connect — try the next.
   * If EVERY kind fails, return null — the caller decides whether that skips
   * the stage (failStage) or is survivable (loaded latency).
   *
   * Order is fixed (see #transportOrder): latency roles try webtransport then
   * websocket; transfer/bidi roles try webtransport then xhr-stream. The sketch
   * below shows the control flow a real implementation fills in.
   */
  #negotiateTransport(role: TransportRole): TransportKind | null {
    const host = this.#host!;
    for (const kind of this.#transportOrder(role)) {
      host.reportTransport({ kind, role, status: "negotiating" });
      // xhr-stream "establishes" the moment the server advertises it — the real
      // TCP connect happens when #primeTransfer opens the fetch, and a connect
      // failure there surfaces as a stream error (handled in #onWorkerError).
      // webtransport/websocket are not serviced yet (Stage 4–5), so they fail
      // negotiation here and we fall through to the next kind.
      if (this.#transportAvailable(kind)) {
        host.reportTransport({ kind, role, status: "established" });
        this.#activeTransport = kind;
        return kind;
      }
      host.reportTransport({ kind, role, status: "failed", detail: "not advertised by server" });
    }
    return null;
  }

  /** Whether the server's advertised capabilities can service `kind` right now. */
  #transportAvailable(kind: TransportKind): boolean {
    const t = this.#capabilities?.transports;
    if (!t) return false;
    switch (kind) {
      case "xhr-stream":
        return t.xhrStream;
      case "websocket":
        return t.websocket;
      case "webtransport":
        return t.webtransport;
    }
  }

  /** The transport kinds to try for a role, most-preferred first. Webtransport
   *  is always attempted first when the server advertises it (Stage 4–5), then
   *  the serviced fallback: websocket for latency roles, xhr-stream for transfer
   *  and bidirectional roles. Until webtransport lands it fails negotiation and
   *  we fall through to the fallback — the path the engine actually runs today
   *  (websocket latency + fetch/xhr-stream transfer). */
  #transportOrder(role: TransportRole): TransportKind[] {
    return role === "latency"
      ? ["webtransport", "websocket"]
      : ["webtransport", "xhr-stream"];
  }

  /* ================= PRIME (warmup window) — open, don't measure ================= */
  /** Open `config.parallelStreams` transfer stream(s) for `dir` over `kind`
   *  (`GET {path}/download?bytes=N` for "down", `POST {path}/upload` streamed body
   *  for "up", or webtransport) and run priming bytes to warm the path (TCP
   *  congestion window / BBR / TLS) — pushing NOTHING into the core. The stream(s)
   *  stay open for #measureTransfer to start measuring on the SAME connection. */
  /** Parallel POST lanes for a stage, derived from (phase, features, transport) —
   *  no manual count. On HTTP/1.1 each lane is its own TCP connection filling the
   *  BDP, so we carve them from the per-origin pool after reserving the buses this
   *  phase needs (#laneBudget). On a multiplexed transport every lane shares ONE
   *  congestion window, so extra lanes buy no throughput — cap low. The configured
   *  `parallelStreams` is only an upper ceiling (#laneCeiling), never a target. */
  #laneBudget(activity: PhaseActivity, kind: TransportKind): number {
    if (kind !== "xhr-stream") return Math.min(2, this.#laneCeiling()); // multiplexed: one fat conn
    const buses = (this.#needsPings(activity) ? 1 : 0) + (activity.transfer.includes("up") ? 1 : 0);
    const lanes = BROWSER_CONN_BUDGET - buses;
    return Math.max(1, Math.min(lanes, this.#laneCeiling()));
  }

  /** Advanced upper bound on lanes (repurposed `parallelStreams`); 0/unset ⇒ the
   *  full connection budget, so the derived policy governs unconstrained. */
  #laneCeiling(): number {
    const max = this.#host!.config!.parallelStreams;
    return max > 0 ? max : BROWSER_CONN_BUDGET;
  }

  #primeTransfer(kind: TransportKind, dir: FlowDirection, activity: PhaseActivity): void {
    if (kind !== "xhr-stream") throw NOT_IMPL(`primeTransfer:${kind}`); // wt = Stage 5

    // A stage that names both lanes (bidirectional) would call this twice; the
    // single pool can't serve two directions at once — that's Stage 6.
    if (this.#workers.length) throw NOT_IMPL("bidirectional transfer");

    const cfg = this.#host!.config!;
    const base = resolveBase(this.#resolveEndpoint(cfg.endpoint));
    this.#laneCount = this.#laneBudget(activity, kind);
    const streams = this.#laneCount;
    // Bound the stagger so the last lane (index laneCount−1) still spawns within
    // half the warmup; 0 when there's no warmup (lanes spawn together rather than
    // bleeding into the measured window).
    this.#laneStaggerMs =
      streams > 1
        ? Math.min(LANE_STAGGER_MS, Math.floor((cfg.duration.warmupMs * 0.5) / (streams - 1)))
        : 0;
    this.#dir = dir;
    // Experimental: the download worker requests adaptive chunks itself, so omit the
    // baked-in ?bytes= and let it append &bytes=N per fetch (see download-worker.ts).
    this.#chunkDownload = dir === "down" && cfg.experimentalChunkedDownload;

    // Download streams the body down (?bytes=N to size it); upload streams a
    // generated body up (no size — the worker generates until the stage stops).
    // Upload gets its per-stage id asynchronously below before opening lanes.
    const url = (i: number, uploadId?: string): string => {
      const cb = `${this.#cbSeed}-${i}`;
      if (dir === "down") {
        const path = this.#capabilities?.endpoints.download ?? "/download";
        return this.#chunkDownload
          ? `${base}${path}?cb=${cb}`
          : `${base}${path}?bytes=${PER_STREAM_BYTES}&cb=${cb}`;
      }
      const path = this.#capabilities?.endpoints.upload ?? "/upload";
      const idParam = uploadId ? `&id=${encodeURIComponent(uploadId)}` : "";
      return `${base}${path}?cb=${cb}${idParam}`;
    };

    this.#workers = [];
    this.#streamUrls = [];
    this.#laneRetry = [];
    this.#laneTimers = [];
    this.#transferActive = true;
    this.#stageSawBytes = false;
    this.#testId = null;

    if (dir === "up") {
      void this.#primeUploadTransfer(base, streams, url);
      return;
    }

    for (let i = 0; i < streams; i++) {
      this.#streamUrls[i] = url(i);
      this.#spawnLaneStaggered(i);
    }
    // Workers start now (warming TCP cwnd). Download worker progress is tagged
    // seq=0 during warmup and ignored; #measureTransfer opens a new epoch and
    // resets the worker-side batch so no warmup bytes bleed into measurement.
    // Upload workers report no bytes (only `alive`) and the server count accrues
    // instead. Either way #measureTransfer starts the measurement path.
  }

  async #primeUploadTransfer(
    base: string,
    streams: number,
    url: (i: number, uploadId?: string) => string,
  ): Promise<void> {
    let id: string;
    try {
      id = await this.#mintUploadSession(base);
    } catch (cause) {
      if (!this.#transferActive) return; // aborted/teardown while the warmup request was in flight
      void cause;
      this.#host!.failStage("upload", "protocol-error", "upload session request failed");
      return;
    }
    if (!this.#transferActive || this.#dir !== "up") return;
    this.#testId = id;
    for (let i = 0; i < streams; i++) {
      this.#streamUrls[i] = url(i, id);
      this.#spawnLaneStaggered(i);
    }
    // Open the /ws/upload progress socket after the token exists. It is still part
    // of warmup; measurement starts later via #measureTransfer.
    this.#primeUploadProgress();
  }

  async #mintUploadSession(base: string): Promise<string> {
    const path = this.#capabilities?.endpoints.uploadSession ?? "/upload/session";
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
        headers: this.#opts.authToken
          ? { authorization: `Bearer ${this.#opts.authToken}` }
          : undefined,
      });
      if (!res.ok) throw new Error(`upload session returned HTTP ${res.status}`);
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

  /** Spawn the /ws/upload progress worker for the up stage. Resets the server
   *  window anchors. The server count is the SOLE upload byte source now (no
   *  client-side onprogress fallback): if there is no minted id or the server does
   *  not advertise WebSocket, no server bytes ever arrive, so the up stage produces
   *  no samples and the core's stall watchdog ends it cleanly (max-stall →
   *  connection-lost) rather than shipping a client-counted number. */
  #primeUploadProgress(): void {
    this.#srvN = 0;
    this.#srvPrevN = 0;
    this.#srvPrevT = 0;
    this.#srvStartN = 0;
    this.#srvStartT = 0;
    this.#srvHaveStart = false;

    const caps = this.#capabilities;
    if (!this.#testId) return; // session mint already skipped the stage
    if (!caps?.transports.websocket) {
      this.#host!.failStage(
        "upload",
        "transport-unavailable",
        "server offers no WebSocket progress bus — upload can't be measured",
      );
      return;
    }

    const wsUpload = caps.endpoints.wsUpload ?? "/ws/upload";
    const url =
      this.#resolveWsBase(this.#host!.config!.endpoint) +
      wsUpload +
      `?id=${encodeURIComponent(this.#testId)}`;
    const w = new Worker(new URL("./workers/upload-progress-worker.ts", import.meta.url), {
      type: "module",
    });
    w.onmessage = (e: MessageEvent<ProgressOutMsg>): void => this.#onProgressMessage(e.data);
    w.onerror = (): void => {
      /* the worker owns reconnect; a hard worker error just means no server bytes
       * until it recovers, which the stall watchdog already covers. */
    };
    w.postMessage({ type: "start", url });
    this.#progressWorker = w;
  }

  /** Spawn lane `i` at prime time, staggered by LANE_STAGGER_MS per index so the
   *  lanes don't slow-start in lockstep. Lane 0 is immediate; later lanes fire from
   *  #laneTimers[i] (which #teardownTransfer clears) within the warmup window. A
   *  lane can't be stagger-pending and restart-pending at once, so sharing the slot
   *  is safe. The URL is already stored before this runs. */
  #spawnLaneStaggered(i: number): void {
    const delay = i * this.#laneStaggerMs;
    if (delay <= 0) {
      this.#spawnWorker(i);
      return;
    }
    this.#laneTimers[i] = setTimeout(() => {
      this.#laneTimers[i] = null;
      if (this.#transferActive) this.#spawnWorker(i);
    }, delay);
  }

  /** Open (or re-open) the worker for stream `i` against its stored URL. The
   *  worker script is chosen by the active direction; both speak the same
   *  start/stop ⇄ progress/error protocol. */
  #spawnWorker(i: number): void {
    const w =
      this.#dir === "down"
        ? new Worker(new URL("./workers/download-worker.ts", import.meta.url), { type: "module" })
        : new Worker(new URL("./workers/upload-worker.ts", import.meta.url), { type: "module" });
    w.onmessage = (e: MessageEvent) => this.#onWorkerMessage(e.data, i);
    w.onerror = (e: ErrorEvent) => this.#onWorkerError(i, e.message || "worker error");
    // `debug`/`id` only drive the worker's own verbose per-stream logging.
    // `streams` lets the upload worker split its total payload budget per stream
    // (so the in-flight reservoir is constant across stream counts); download
    // ignores it.
    w.postMessage({
      type: "start",
      url: this.#streamUrls[i],
      debug: debugEnabled(),
      id: i,
      // Derived lane count so the per-worker payload split matches the real lanes.
      streams: this.#laneCount,
      // Download-only experimental chunked mode (ignored by the upload worker).
      chunk: this.#chunkDownload,
    });
    if (this.#measuring && this.#dir === "down") {
      w.postMessage({ type: "measure", seq: this.#downloadMeasureSeq });
    }
    this.#workers[i] = w;
  }

  /** Open the latency (ping) channel over `kind` and warm it. Spawns the
   *  dedicated ping worker (which owns the WebSocket + the whole ping algorithm),
   *  hands it the tuning, and lets it send warmup pings — pushing NOTHING into
   *  the core. #measureLatency flips reporting on over the SAME warmed socket. */
  #primeLatencyChannel(kind: TransportKind, isLatencyStage = false): void {
    if (kind !== "websocket") throw NOT_IMPL(`primeLatencyChannel:${kind}`); // wt = Stage 5

    const cfg = this.#host!.config!;
    const wsPing = this.#capabilities?.endpoints.wsPing ?? "/ws/ping";
    const url = this.#resolveWsBase(cfg.endpoint) + wsPing;
    const intervalMs = PING_INTERVAL[cfg.pingConcurrency];

    this.#latencyUnderLoad = false;
    this.#pingActive = true;
    // The idle latency stage has no byte lanes to prove the link — bound how
    // long the channel gets to deliver its first pong before the stage skips.
    if (isLatencyStage) {
      this.#pingEstablishTimer = setTimeout(() => {
        this.#pingEstablishTimer = null;
        this.#host!.failStage("latency", "connection-lost", "ping connection could not be established");
      }, PING_ESTABLISH_TIMEOUT_MS);
    }
    const w = new Worker(new URL("./workers/ping-worker.ts", import.meta.url), { type: "module" });
    w.onmessage = (e: MessageEvent<PingOutMsg>): void => this.#onPingMessage(e.data);
    w.onerror = (e: ErrorEvent): void => this.#onPingMessage({ type: "stall", detail: e.message || "ping worker error" });
    w.postMessage({
      type: "start",
      url,
      intervalMs,
      maxInFlight: PING_MAX_IN_FLIGHT,
      minGapMs: PING_MIN_GAP_MS,
      reportGapMs: PING_REPORT_GAP_MS,
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
    });
    this.#pingWorker = w;
  }

  /** Resolve the ws(s):// base for the latency bus: prefer the advertised h1
   *  origin, else the given endpoint base, else same-origin — each mapped
   *  http→ws. Takes the endpoint explicitly so it is safe during probe(), before
   *  the run config exists. */
  #resolveWsBase(endpoint?: RunnerConfig["endpoint"]): string {
    const h1 = this.#capabilities?.origins.h1;
    if (h1) return httpToWs(h1);
    const base = resolveBase(this.#resolveEndpoint(endpoint));
    if (base) return httpToWs(base);
    return httpToWs(location.origin);
  }

  /** Handle a message from the ping worker. The worker reports already-computed
   *  RTTs; the runner just tags underLoad and forwards. stall/resume bracket a
   *  reconnect — surfaced to the core ONLY for the idle latency stage; during a
   *  transfer stage the byte lanes drive link health (and a ping gap must never
   *  freeze throughput accrual), so loaded-latency reconnects pass silently. */
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
          this.#host!.stall({ reason: "connection-lost", transport: "websocket", detail: msg.detail });
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
   *  on-receive chain is OFF and the in-flight window tiny: the pacer alone
   *  drives sends, so this can never ping (or update the UI) faster than
   *  once per interval. */
  #startIdleKeepalive(
    endpoint?: RunnerConfig["endpoint"],
    intervalMs = IDLE_PING_INTERVAL_MS,
  ): void {
    if (this.#idleActive || !this.#capabilities?.transports.websocket) return;
    const wsPing = this.#capabilities?.endpoints.wsPing ?? "/ws/ping";
    const url = this.#resolveWsBase(endpoint) + wsPing;
    this.#idleActive = true;
    // Treat connectivity as unknown until this (fresh) worker proves the link:
    // its first samples then emit a "connected" edge. Crucial after a
    // connection-lost failure — the store latched the pulse offline, and
    // without this edge a link that recovered before the worker's first stall
    // would never un-latch it.
    this.#idleOffline = true;
    const w = new Worker(new URL("./workers/ping-worker.ts", import.meta.url), { type: "module" });
    w.onmessage = (e: MessageEvent<PingOutMsg>): void => this.#onIdlePingMessage(e.data);
    w.onerror = (e: ErrorEvent): void => {
      // Worker died without ever running its reconnect loop — most commonly the
      // script fetch itself failed because the (bundle-serving) server is down,
      // e.g. restarting the keepalive right after a connection-lost run. Report
      // offline and retry the SPAWN until one sticks (the in-worker reconnect
      // loop only exists once the script loads).
      this.#onIdlePingMessage({ type: "stall", detail: e.message || "idle ping worker error" });
      this.#scheduleIdleRespawn(endpoint, intervalMs);
    };
    w.postMessage({
      type: "start",
      url,
      intervalMs,
      maxInFlight: 2,
      minGapMs: PING_MIN_GAP_MS,
      reportGapMs: 0, // paced sends are already sparse — report every sample
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
    });
    // Report immediately (no warmup window), pacer-driven only — the
    // on-receive chain would otherwise ping at ~1 kHz.
    w.postMessage({ type: "measure", chainOnReceive: false });
    this.#idleWorker = w;
  }

  /** Stop the idle keepalive — a real run is starting (onRunStart), or the
   *  app is tearing down. Idempotent. */
  #stopIdleKeepalive(): void {
    this.#idleActive = false;
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
  #scheduleIdleRespawn(endpoint?: RunnerConfig["endpoint"], intervalMs?: number): void {
    if (!this.#idleActive || this.#idleRespawnTimer) return;
    this.#idleRespawnTimer = setTimeout(() => {
      this.#idleRespawnTimer = null;
      if (!this.#idleActive) return; // a run started (or teardown) meanwhile
      this.#stopIdleKeepalive();
      this.#startIdleKeepalive(endpoint, intervalMs);
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
            if (this.#probeCollect.rtts.length >= PROBE_PING_COUNT) this.#probeCollect.finish();
          }
          this.#host!.emit({
            type: "latency",
            sample: { t: 0, rttMs: s.rtt, underLoad: false, lost: s.lost, phase: "idle" },
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
    void dir; // direction was fixed in #primeTransfer (#dir); the pool is warm.
    // Reuse the SAME workers primed during warmup — never re-spawn (that throws
    // away the warmed congestion window). Just open the measurement window:
    // discard whatever accrued during warmup and start aggregating.
    this.#measuring = true;
    this.#pendingLaneBytes = Array(this.#laneCount).fill(0);
    this.#pendingLaneElapsedSec = Array(this.#laneCount).fill(0);
    if (this.#dir === "down") {
      this.#downloadMeasureSeq++;
      for (const w of this.#workers) w?.postMessage({ type: "measure", seq: this.#downloadMeasureSeq });
    }
    this.#dbgWinBytes = 0;
    this.#dbgLastLog = performance.now();
    // Anchor the server-authoritative window at measure-start so warmup bytes are
    // excluded from both the live curve delta and the totals-based headline. The
    // start (startN/startT) is captured on the first measured server byte.
    this.#srvHaveStart = false;
    this.#srvPrevN = this.#srvN;
    this.#aggTimer = setInterval(() => this.#aggregate(), THROUGHPUT_CADENCE_MS);
  }

  /** Aggregation tick: sum the byte deltas all workers reported since the last
   *  tick into one real sample tagged with the active direction. For download,
   *  each lane contributes bytes / that lane's own receive interval, then the
   *  lane rates are summed. Pushes nothing on an empty window — dead air is
   *  never a synthesized sample (principle 1); the core's stall watchdog covers
   *  a genuine gap. */
  #aggregate(): void {
    const now = performance.now();
    let delta = 0;
    let bytesPerSec = 0;
    for (let i = 0; i < this.#pendingLaneBytes.length; i++) {
      const laneBytes = this.#pendingLaneBytes[i] ?? 0;
      const laneSec = this.#pendingLaneElapsedSec[i] ?? 0;
      this.#pendingLaneBytes[i] = 0;
      this.#pendingLaneElapsedSec[i] = 0;
      if (laneBytes <= 0 || laneSec <= 0) continue;
      delta += laneBytes;
      bytesPerSec += laneBytes / laneSec;
    }
    if (delta > 0 && bytesPerSec > 0) {
      this.#host!.ingestThroughput(this.#dir, bytesPerSec, delta);
    }
    // Verbose: the pool's combined raw rate, 1 Hz. This is the sum the core
    // then smooths (see core:throughput) — comparing this to the per-worker
    // raw logs shows whether aggregation loses anything, and to the server
    // figure whether bytes are lost between the wire and JS.
    if (debugEnabled()) {
      this.#dbgWinBytes += delta;
      const dt = now - this.#dbgLastLog;
      if (dt >= 1000) {
        const active = this.#workers.reduce((n, w) => n + (w ? 1 : 0), 0);
        dlog("realrunner:aggregate", `${this.#dir} pool`, {
          rate: fmtRate(this.#dbgWinBytes / (dt / 1000)),
          tick: fmtRate(bytesPerSec),
          window: fmtBytes(this.#dbgWinBytes),
          streams: active,
          dt: fmtMs(dt),
        });
        this.#dbgWinBytes = 0;
        this.#dbgLastLog = now;
      }
    }
  }

  /** A transfer worker reported liveness, bytes, or a stream error.
   *   • download `progress` → client-counted bytes drive the live curve: accrue
   *     into the aggregation window and clear any open stall.
   *   • upload `alive` → one POST completed; reset the lane's restart counter. The
   *     server /ws/upload count is the SOLE upload byte source, so an upload lane
   *     reports NO bytes and never drives the curve or resumes a stall here (the
   *     progress worker owns the up curve + its stall/resume — see #onProgressMessage).
   *   • `error` → stall + restart that single lane (#onWorkerError). */
  #onWorkerMessage(
    msg:
      | { type: "progress"; bytes: number; elapsedMs?: number; seq?: number }
      | { type: "alive" }
      | { type: "error"; recoverable: boolean; detail: string },
    i: number,
  ): void {
    if (msg.type === "progress") {
      if (
        this.#dir === "down" &&
        ("seq" in msg ? msg.seq !== this.#downloadMeasureSeq || msg.seq <= 0 : true)
      ) {
        return;
      }
      this.#laneRetry[i] = 0; // a real send proves this lane recovered
      this.#stageSawBytes = true;
      const elapsedMs = msg.elapsedMs ?? THROUGHPUT_CADENCE_MS;
      this.#pendingLaneBytes[i] = (this.#pendingLaneBytes[i] ?? 0) + msg.bytes;
      this.#pendingLaneElapsedSec[i] =
        (this.#pendingLaneElapsedSec[i] ?? 0) + elapsedMs / 1000;
      if (this.#stalled) {
        this.#host!.resume();
        this.#stalled = false;
      }
    } else if (msg.type === "alive") {
      this.#laneRetry[i] = 0; // a completed POST proves this upload lane recovered
      this.#stageSawBytes = true;
    } else {
      this.#onWorkerError(i, msg.detail, msg.recoverable);
    }
  }

  /** Handle a transfer lane failure (download or upload). Recoverable (the common
   *  case: a dropped connection) → stall once, then re-open the lane so a real
   *  sample resumes it. Only call fail() when the drop is genuinely unrecoverable. */
  #onWorkerError(i: number, detail: string, recoverable = true): void {
    // Ignore late errors after teardown (a stop()/terminate races the worker).
    if (!this.#transferActive) return;
    if (!recoverable) {
      this.#host!.fail("connection-lost", `${this.#dir} stream ${i} failed: ${detail}`, detail);
      return;
    }
    if (this.#measuring && !this.#stalled) {
      this.#host!.stall({
        reason: "connection-lost",
        transport: this.#activeTransport ?? undefined,
        detail,
      });
      this.#stalled = true;
    }
    // Tear the lane down now; re-open it after a backoff so a persistently-
    // failing stream can't spin a tight respawn loop (re-creating workers +
    // re-fetching wasm hundreds of times/sec). Give up the run once a lane
    // exhausts its restarts — the core's max-stall timeout also bounds patience.
    this.#workers[i]?.terminate();
    this.#workers[i] = null;
    this.#laneRetry[i] = (this.#laneRetry[i] ?? 0) + 1;
    // Never moved a byte this stage + repeated instant failures ⇒ the endpoint
    // is unreachable/unsupported. Skip the stage instead of stalling for 20 s.
    if (!this.#stageSawBytes && this.#laneRetry[i] > EARLY_FAIL_RESTARTS) {
      const stage = this.#dir === "down" ? "download" : "upload";
      this.#host!.failStage(stage, "connection-lost", `${this.#dir}link connection could not be established`);
      return;
    }
    if (this.#laneRetry[i] > LANE_MAX_RESTARTS) {
      this.#host!.fail("connection-lost", `${this.#dir} stream ${i} kept dropping: ${detail}`, detail);
      return;
    }
    this.#laneTimers[i] = setTimeout(() => {
      this.#laneTimers[i] = null;
      if (this.#transferActive) this.#spawnWorker(i);
    }, LANE_RESTART_BACKOFF_MS);
  }

  /** A message from the /ws/upload progress worker. The server count is the SOLE
   *  upload byte source: `bytes`/`complete` feed the live curve AND the totals-based
   *  headline. Because there is no client-side fallback, the socket dropping is the
   *  only thing that can leave the up stage without samples — so the worker's
   *  `stall`/`resume` (bracketing its reconnect) are forwarded to the core to freeze
   *  measured-time across the gap. This is the watchdog story post-onprogress: while
   *  the socket is up, the 100 ms frames are the heartbeat (each advancing frame →
   *  ingestThroughput → noteRealSample); while it is down, measured-time is frozen,
   *  and on reconnect the cumulative count + the server's active-time denominator
   *  self-heal the headline. The POST lanes are separate connections, so a progress-
   *  socket drop doesn't stop the transfer: the server keeps draining and accruing
   *  active-time, and the catch-up Δn / Δactive on reconnect IS the true rate over
   *  the gap — no client-side counting anywhere. */
  #onProgressMessage(msg: ProgressOutMsg): void {
    if (!this.#transferActive || this.#dir !== "up") return; // late message after teardown
    if (msg.type === "stall") {
      // The progress socket dropped: no server bytes until it reconnects. Freeze
      // measured-time so the gap doesn't count against the rate (rather than wait
      // for the core's silence watchdog to notice ~1.5 s later).
      if (this.#measuring && !this.#stalled) {
        this.#host!.stall({ reason: "connection-lost", transport: "websocket", detail: msg.detail });
        this.#stalled = true;
      }
      return;
    }
    if (msg.type === "resume") {
      if (this.#stalled) {
        this.#host!.resume();
        this.#stalled = false;
      }
      return;
    }
    if (msg.type !== "bytes" && msg.type !== "complete") return; // open: nothing to do

    // `srvT` is the server's ACTIVE measurement clock (ns) at which the server
    // sampled this count — ns bytes were actually being drained, dead zones excluded,
    // NOT this thread's frame-arrival time and NOT a wall span. Every rate below
    // divides server bytes by server active-time, so neither the live curve nor the
    // headline can be skewed by local tick cadence, event-loop lag, arrival jitter,
    // or an idle gap (establishment, reconnect, stall) — the server already excised it.
    const srvT = msg.t;
    if (msg.n > this.#srvN) {
      this.#srvN = msg.n; // cumulative + monotonic guard
      this.#stageSawBytes = true;
    }
    if (!this.#measuring) return; // warmup bytes are excluded from the window

    if (!this.#srvHaveStart) {
      this.#srvHaveStart = true;
      this.#srvStartN = this.#srvN;
      this.#srvStartT = srvT;
      this.#srvPrevN = this.#srvN;
      this.#srvPrevT = srvT;
    }
    // Server bytes drive the live curve directly from the server stream — never
    // via the local #aggregate tick (whose fixed cadence would skew the rate).
    // Each sample is the byte delta between two server frames divided by the
    // server ACTIVE-time between those same frames, so the rate is correct at
    // any tick/frame cadence, and a reconnect gap self-heals: no dead-zone time
    // enters active-time, so the backlog delta over the gap is the true rate.
    const delta = this.#srvN - this.#srvPrevN;
    if (delta > 0) {
      const frameSec = (srvT - this.#srvPrevT) / 1e9;
      this.#srvPrevN = this.#srvN;
      this.#srvPrevT = srvT;
      if (frameSec > 0) this.#host!.ingestThroughput("up", delta / frameSec, delta);
      if (this.#stalled) {
        this.#host!.resume();
        this.#stalled = false;
      }
    }
    // Totals-based authoritative headline over the measured window — one ratio,
    // immune to per-frame arrival jitter (unlike a mean of per-tick rates), using the
    // server's ACTIVE measurement time between the first and last server sample (idle
    // gaps already excised server-side, so this is "time to get Δn bytes", not wall).
    const dtSec = (srvT - this.#srvStartT) / 1e9;
    if (dtSec > 0) {
      this.#host!.reportUploadServerRate((this.#srvN - this.#srvStartN) / dtSec);
    }
  }

  /** Stop the /ws/upload worker: it sends BYE (the server's authoritative
   *  finalizer) then closes; we terminate after a grace so BYE + UPLOAD_COMPLETE
   *  can flush. The client headline was already set from the last reported rate,
   *  so we never block on it. Idempotent. */
  #teardownProgress(): void {
    const w = this.#progressWorker;
    this.#progressWorker = null;
    if (!w) return;
    w.postMessage({ type: "stop" });
    setTimeout(() => w.terminate(), PROGRESS_BYE_GRACE_MS);
  }

  /** Stop + terminate the worker pool, the aggregation timer, and any pending
   *  lane-restart backoffs. Idempotent. */
  #teardownTransfer(): void {
    this.#transferActive = false;
    if (this.#aggTimer !== null) {
      clearInterval(this.#aggTimer);
      this.#aggTimer = null;
    }
    for (const t of this.#laneTimers) if (t) clearTimeout(t);
    for (const w of this.#workers) {
      if (!w) continue;
      w.postMessage({ type: "stop" });
      w.terminate();
    }
    // Stop the progress worker AFTER the POST lanes — BYE must follow the lanes
    // ending so the server's final count includes everything they drained.
    this.#teardownProgress();
    this.#workers = [];
    this.#streamUrls = [];
    this.#laneRetry = [];
    this.#laneTimers = [];
    this.#measuring = false;
    this.#pendingLaneBytes = [];
    this.#pendingLaneElapsedSec = [];
  }

  /** Begin measuring on the already-open ping channel (opened in
   *  #primeLatencyChannel). RTT = now − sent; an unacked / timed-out ping is
   *  `lost`. Push at the config.pingConcurrency interval via
   *  host.ingestLatency(rtt, underLoad, lost) — `underLoad` is true when the
   *  pings run concurrently with a transfer (bufferbloat). */
  #measureLatency(underLoad: boolean): void {
    // The worker primed in #primeLatencyChannel is already pinging on a warm
    // socket; just flip reporting on (never re-spawn — that would throw away the
    // warmup). underLoad tags every forwarded sample: true when these pings run
    // concurrently with a transfer (bufferbloat), false for the idle stage.
    this.#latencyUnderLoad = underLoad;
    const cfg = this.#host!.config!;
    this.#pingWorker?.postMessage(
      underLoad
        ? { type: "measure", chainOnReceive: false, maxInFlight: PING_LOADED_MAX_IN_FLIGHT, intervalMs: PING_LOADED_INTERVAL_MS }
        : { type: "measure", chainOnReceive: true, maxInFlight: PING_MAX_IN_FLIGHT, intervalMs: PING_INTERVAL[cfg.pingConcurrency] },
    );
  }

  /* ================= OPTIONAL SEAMS ================= */
  /**
   * OPTIONAL — list the endpoints this backend can target, for a future
   * server-selection UI (TARGET: `GET {path}/servers`, §14.4). Remove the method
   * to drop it from the surface; the core then reports an empty list.
   */
  async listServers(): Promise<ServerCandidate[]> {
    throw NOT_IMPL("listServers");
  }

  /**
   * OPTIONAL — fallback idle RTT, used only when a run yields no usable latency
   * samples. A real engine can return its last preflight ping; 0 means "no hint".
   */
  idleHintMs(): number {
    return 0;
  }

  /**
   * OPTIONAL on the contract — a real transport has no synthetic knob to
   * perturb, so this dev-only hook is a no-op. Remove it to drop it entirely.
   */
  injectAnomaly(_a: RunnerAnomaly): void {
    /* no-op: real transport has no synthetic anomaly to inject */
  }

  /* ---------- internal ---------- */
  /** Cancel in-flight I/O and release the AbortController. */
  #closeAll(): void {
    this.#teardownTransfer();
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
