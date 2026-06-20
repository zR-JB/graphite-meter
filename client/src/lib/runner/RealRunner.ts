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
  ServerCandidate,
  TransportKind,
  TransportRole,
  FlowDirection,
  PhaseActivity,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import type { Preflight } from "../api/preflight";

/** Resolve the fetch base URL for the backend. `host:"auto"` (or empty) means
 *  same-origin (relative requests) — the Stage-1 case where the Go server serves
 *  both the app and the API. A concrete host builds an absolute origin. */
function resolveBase(endpoint?: RunnerConfig["endpoint"]): string {
  if (!endpoint || endpoint.host === "auto" || endpoint.host === "") return "";
  const scheme = endpoint.port === 443 ? "https" : "http";
  return `${scheme}://${endpoint.host}:${endpoint.port}`;
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
  new Error(`RealBackend.${method} not implemented — see docs/REAL_RUNNER.md`);

/** Throughput push cadence: aggregate worker deltas and push ~16 Hz (mirrors
 *  the dummy's THROUGHPUT_CADENCE_MS so both engines feel identical). */
const THROUGHPUT_CADENCE_MS = 60;

/** Bytes requested per download stream. Large enough that one request lasts the
 *  whole measured window on a fast link (the server clamps and we abort at stage
 *  end); the worker also re-fetches if a stream ends early. */
const PER_STREAM_BYTES = 8 * 1024 * 1024 * 1024; // 8 GiB

/** Backoff before re-opening a dropped lane, so a persistently-failing stream
 *  can't spin a tight respawn loop (re-creating workers + re-fetching wasm). */
const LANE_RESTART_BACKOFF_MS = 300;
/** Give up a lane after this many consecutive restarts (≈12 s at the backoff);
 *  the core's max-stall timeout also bounds total patience. */
const LANE_MAX_RESTARTS = 40;

/** Order-preserving de-dup for building a transport preference list. */
function unique<T>(xs: T[]): T[] {
  return [...new Set(xs)];
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

  /* ---- transfer stage state (Stage 2 download, Stage 3 upload) ---- */
  /** The active transfer direction for the current stage's worker pool. */
  #dir: FlowDirection = "down";
  /** One worker per parallel stream, indexed by stream number. Download workers
   *  read-and-count; upload workers generate-and-stream. Same message protocol. */
  #workers: (Worker | null)[] = [];
  /** The fetch URL each stream worker (re)starts against, by index. */
  #streamUrls: string[] = [];
  /** Bytes received across all workers since the last aggregation tick. */
  #pendingBytes = 0;
  /** performance.now() at the last aggregation tick — the denominator of the
   *  truthful per-tick rate (real bytes / real elapsed). */
  #lastAggAt = 0;
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
  /** Per-run cache-buster seed, so `?cb=` is unique across runs and streams. */
  #cbSeed = "";

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
   * TARGET: `GET {endpoint.path}/preflight` (a.k.a. /config) — §14.4.
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
      res = await fetch(`${base}/preflight`, {
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
    // #host is captured in attach() for later-stage sample pushing (pre-test
    // pings land here once a ping endpoint exists, Stage 4); referenced now so
    // the field isn't flagged write-only while the I/O methods are still stubs.
    void this.#host;
    return {
      clientIp: pf.clientIp,
      server: {
        name: pf.server.name,
        host: pf.server.host,
        port: pf.server.port,
        location: pf.server.location,
      },
      preTestPingMs: pf.preTestPingMs,
      engineVersion: pf.engineVersion,
      protocolNegotiated: pf.protocolNegotiated,
    };
  }

  /** The server capabilities captured by the last successful probe (or null
   *  before probing). Consumed by later-stage transport negotiation. */
  get capabilities(): Preflight["capabilities"] | null {
    return this.#capabilities;
  }

  /* ================= LIFECYCLE (core → backend) ================= */
  /**
   * A run is starting. Open a fresh AbortController so onAbort can cancel
   * everything; reset any per-run state. Do NOT open transfer connections yet —
   * that happens per stage in onStageBegin.
   */
  onRunStart(config: RunnerConfig): void {
    void this.#resolveEndpoint(config.endpoint);
    this.#abort = new AbortController();
    this.#activeTransport = null;
    this.#teardownTransfer(); // clear any leftover lanes from a prior run
    this.#measuring = false;
    this.#stalled = false;
    this.#pendingBytes = 0;
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
    const kind = this.#negotiateTransport(activity.stage);
    if (!kind) return; // negotiation already raised host.fail("transport-unavailable")
    for (const dir of activity.transfer) this.#primeTransfer(kind, dir);
    if (this.#needsPings(activity)) this.#primeLatencyChannel(kind);
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
    // download that means stopping + terminating the worker pool.
    this.#teardownTransfer();
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
   * If EVERY kind fails, raise the terminal host.fail("transport-unavailable",
   * …) and return null (the caller must NOT open anything).
   *
   * Order derivation: config.transport.transfer is "webtransport"|"xhr-stream"
   * and config.transport.latency is "webtransport"|"websocket"; latency roles
   * use the latency kinds, transfer/bidi roles use the transfer kinds. The
   * sketch below shows the control flow a real implementation fills in.
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
    host.fail("transport-unavailable", `No transport could be established for ${role}`);
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

  /** The transport kinds to try for a role, most-preferred first. Latency roles
   *  use the latency kinds (webtransport → websocket); transfer + bidirectional
   *  roles use the transfer kinds (webtransport → xhr-stream). Webtransport is
   *  always attempted first when supported, then the configured fallback. */
  #transportOrder(role: TransportRole): TransportKind[] {
    const cfg = this.#host!.config!;
    // Webtransport is always preferred when available; latency roles fall back
    // to the configured ws kind, transfer/bidi roles to xhr-stream. xhr-stream
    // is appended unconditionally so a webtransport-preferred config still has a
    // working fallback (the Stage-2 path: webtransport unadvertised → xhr-stream).
    return role === "latency"
      ? unique(["webtransport", cfg.transport.latency])
      : unique(["webtransport", cfg.transport.transfer, "xhr-stream"]);
  }

  /* ================= PRIME (warmup window) — open, don't measure ================= */
  /** Open `config.parallelStreams` transfer stream(s) for `dir` over `kind`
   *  (`GET {path}/download?bytes=N` for "down", `POST {path}/upload` streamed body
   *  for "up", or webtransport) and run priming bytes to warm the path (TCP
   *  congestion window / BBR / TLS) — pushing NOTHING into the core. The stream(s)
   *  stay open for #measureTransfer to start measuring on the SAME connection. */
  #primeTransfer(kind: TransportKind, dir: FlowDirection): void {
    if (kind !== "xhr-stream") throw NOT_IMPL(`primeTransfer:${kind}`); // wt = Stage 5

    // A stage that names both lanes (bidirectional) would call this twice; the
    // single pool can't serve two directions at once — that's Stage 6.
    if (this.#workers.length) throw NOT_IMPL("bidirectional transfer");

    const cfg = this.#host!.config!;
    const base = resolveBase(this.#resolveEndpoint(cfg.endpoint));
    const streams = Math.max(1, cfg.parallelStreams);
    this.#dir = dir;

    // Download streams the body down (?bytes=N to size it); upload streams a
    // generated body up (no size — the worker generates until the stage stops).
    const url = (i: number): string => {
      const cb = `${this.#cbSeed}-${i}`;
      if (dir === "down") {
        const path = this.#capabilities?.endpoints.download ?? "/download";
        return `${base}${path}?bytes=${PER_STREAM_BYTES}&cb=${cb}`;
      }
      const path = this.#capabilities?.endpoints.upload ?? "/upload";
      return `${base}${path}?cb=${cb}`;
    };

    this.#workers = [];
    this.#streamUrls = [];
    this.#laneRetry = [];
    this.#laneTimers = [];
    this.#transferActive = true;
    for (let i = 0; i < streams; i++) {
      this.#streamUrls[i] = url(i);
      this.#spawnWorker(i);
    }
    // Workers start now (warming TCP cwnd). Their progress accrues into
    // #pendingBytes during warmup but is NOT pushed — #measureTransfer resets the
    // window and starts the aggregation timer.
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
    w.postMessage({ type: "start", url: this.#streamUrls[i] });
    this.#workers[i] = w;
  }

  /** Open the latency (ping) channel over `kind` and optionally exchange a warm-
   *  up ping or two. Push NOTHING yet — #measureLatency starts the real pinging
   *  on this same channel. */
  #primeLatencyChannel(kind: TransportKind): void {
    void kind;
    throw NOT_IMPL("primeLatencyChannel");
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
    this.#pendingBytes = 0;
    this.#lastAggAt = performance.now();
    this.#aggTimer = setInterval(() => this.#aggregate(), THROUGHPUT_CADENCE_MS);
  }

  /** Aggregation tick: sum the byte deltas all workers reported since the last
   *  tick into one real sample tagged with the active direction. Pushes nothing
   *  on an empty window — dead air is never a synthesized sample (principle 1);
   *  the core's stall watchdog covers a genuine gap. */
  #aggregate(): void {
    const now = performance.now();
    const delta = this.#pendingBytes;
    this.#pendingBytes = 0;
    const elapsedSec = (now - this.#lastAggAt) / 1000;
    this.#lastAggAt = now;
    if (delta > 0 && elapsedSec > 0) {
      this.#host!.ingestThroughput(this.#dir, delta / elapsedSec, delta);
    }
  }

  /** A worker reported bytes or a stream error. Progress accrues into the
   *  aggregation window (and clears any open stall); an error stalls + restarts
   *  that single lane. */
  #onWorkerMessage(
    msg: { type: "progress"; bytes: number } | { type: "error"; recoverable: boolean; detail: string },
    i: number,
  ): void {
    if (msg.type === "progress") {
      this.#pendingBytes += msg.bytes;
      this.#laneRetry[i] = 0; // a real sample proves this lane recovered
      if (this.#stalled) {
        this.#host!.resume();
        this.#stalled = false;
      }
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
    if ((this.#laneRetry[i] = (this.#laneRetry[i] ?? 0) + 1) > LANE_MAX_RESTARTS) {
      this.#host!.fail("connection-lost", `${this.#dir} stream ${i} kept dropping: ${detail}`, detail);
      return;
    }
    this.#laneTimers[i] = setTimeout(() => {
      this.#laneTimers[i] = null;
      if (this.#transferActive) this.#spawnWorker(i);
    }, LANE_RESTART_BACKOFF_MS);
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
    this.#workers = [];
    this.#streamUrls = [];
    this.#laneRetry = [];
    this.#laneTimers = [];
    this.#measuring = false;
    this.#pendingBytes = 0;
  }

  /** Begin measuring on the already-open ping channel (opened in
   *  #primeLatencyChannel). RTT = now − sent; an unacked / timed-out ping is
   *  `lost`. Push at the config.pingConcurrency interval via
   *  host.ingestLatency(rtt, underLoad, lost) — `underLoad` is true when the
   *  pings run concurrently with a transfer (bufferbloat). */
  #measureLatency(underLoad: boolean): void {
    void underLoad;
    throw NOT_IMPL("measureLatency");
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
    this.#stalled = false;
    if (this.#abort) {
      this.#abort.abort();
      this.#abort = null;
    }
    this.#activeTransport = null;
  }
}
