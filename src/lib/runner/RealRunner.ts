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
 *  • connection lifecycle — open/prime on onPhaseEnter, close on
 *                    onPhaseExit / onAbort / onComplete. The CORE decides
 *                    WHEN phases change; the backend just reacts.
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
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import type { StagePhase } from "./schedule";

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

export class RealBackend implements RunnerBackend {
  #opts: RealBackendOptions;
  /** The core handle: push samples / emit / report failures + health through it. */
  #host: CoreHost | null = null;
  /** AbortController for in-flight fetches/streams; aborted in onAbort. */
  #abort: AbortController | null = null;
  /** The transport established for the active phase, for stall/fail reporting. */
  #activeTransport: TransportKind | null = null;

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
    void this.#resolveEndpoint(endpoint);
    void this.#opts.authToken;
    void this.#host;
    throw NOT_IMPL("probe");
  }

  /* ================= LIFECYCLE (core → backend) ================= */
  /**
   * A run is starting. Open a fresh AbortController so onAbort can cancel
   * everything; reset any per-run state. Do NOT open transfer connections yet —
   * that happens per stage in onPhaseEnter.
   */
  onRunStart(config: RunnerConfig): void {
    void this.#resolveEndpoint(config.endpoint);
    this.#abort = new AbortController();
    this.#activeTransport = null;
    throw NOT_IMPL("onRunStart");
  }

  /**
   * A phase has begun (the core decided the transition). Negotiate the
   * transport, open/prime the connection(s) it needs, and begin measuring:
   *   • warmup        → prime the following stage's connection(s) (`warmupFor`);
   *                     for transfers, also the latency connection when loaded-
   *                     latency is active (see the warmup contract in contract.ts).
   *   • latency       → #negotiateTransport("latency") then #openLatencyChannel.
   *   • download      → #negotiateTransport("download") then #openDownloadStreams.
   *   • upload        → #negotiateTransport("upload") then #openUploadStreams.
   *   • bidirectional → #negotiateTransport("bidirectional") then #openBidirectional
   *                     — concurrent down + up; push BOTH directions.
   */
  onPhaseEnter(phase: StagePhase | "warmup", warmupFor?: StagePhase): void {
    // Real shape (the stub bodies below throw NOT_IMPL — fill them in to go
    // live). Negotiation runs first; on success it returns the established kind
    // and we open that phase's I/O. A warmup primes its `warmupFor` stage's
    // connection (default latency).
    const role: TransportRole = phase === "warmup" ? (warmupFor ?? "latency") : phase;
    const kind = this.#negotiateTransport(role);
    if (!kind) return; // negotiation already raised host.fail("transport-unavailable")
    switch (phase) {
      case "latency":
        this.#openLatencyChannel(kind);
        break;
      case "download":
        this.#openDownloadStreams(kind);
        break;
      case "upload":
        this.#openUploadStreams(kind);
        break;
      case "bidirectional":
        this.#openBidirectional(kind);
        break;
      case "warmup":
        /* prime warmupFor's connection(s) using `kind` */
        break;
    }
  }

  /** A phase has ended (boundary, early finish, or run end). Close/cancel that
   *  phase's connection(s); the core has already finalized its result. */
  onPhaseExit(phase: StagePhase | "warmup"): void {
    void phase;
    throw NOT_IMPL("onPhaseExit");
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
    const order = this.#transportOrder(role);
    void order;
    // for (const kind of order) {
    //   this.#host!.reportTransport({ kind, role, status: "negotiating" });
    //   try {
    //     await this.#tryOpen(kind, role);            // real connect attempt
    //     this.#host!.reportTransport({ kind, role, status: "established" });
    //     this.#activeTransport = kind;               // remember for stall/fail
    //     return kind;
    //   } catch (cause) {
    //     this.#host!.reportTransport({ kind, role, status: "failed", detail: String(cause) });
    //   }
    // }
    // this.#host!.fail("transport-unavailable", `No transport could be established for ${role}`);
    // return null;
    throw NOT_IMPL("negotiateTransport");
  }

  /** The transport kinds to try for a role, most-preferred first. Latency roles
   *  use the latency kinds (webtransport → websocket); transfer + bidirectional
   *  roles use the transfer kinds (webtransport → xhr-stream). Webtransport is
   *  always attempted first when supported, then the configured fallback. */
  #transportOrder(role: TransportRole): TransportKind[] {
    void role;
    // const cfg = this.#host!.config!;
    // return role === "latency"
    //   ? unique(["webtransport", cfg.transport.latency])
    //   : unique(["webtransport", cfg.transport.transfer]);
    throw NOT_IMPL("transportOrder");
  }

  /* ================= CHANNEL / STREAM OPENING ================= */
  /** Open the latency (ping) channel over `kind`. RTT = now − sent; an unacked /
   *  timed-out ping is `lost`. Push at the config.pingConcurrency interval via
   *  host.ingestLatency(rtt, underLoad=false, lost). */
  #openLatencyChannel(kind: TransportKind): void {
    void kind;
    throw NOT_IMPL("openLatencyChannel");
  }

  /** Open `config.parallelStreams` download streams (`GET {path}/download?bytes=N`
   *  or webtransport). Sum received bytes/sec across streams; push ~16Hz via a
   *  #readLoop per stream. Reuse pings under load → ingestLatency(…, true, …). */
  #openDownloadStreams(kind: TransportKind): void {
    void kind;
    // for (let i = 0; i < this.#host!.config!.parallelStreams; i++)
    //   this.#readLoop(/* reader i */ null, "down");
    this.#readLoop(null, "down");
    throw NOT_IMPL("openDownloadStreams");
  }

  /** Open `config.parallelStreams` upload streams (`POST {path}/upload` streamed
   *  body; server discards). Measure SENT bytes/sec per #readLoop(dir:"up"). */
  #openUploadStreams(kind: TransportKind): void {
    void kind;
    this.#readLoop(null, "up");
    throw NOT_IMPL("openUploadStreams");
  }

  /** Open concurrent download + upload streams for the bidirectional phase; run
   *  #readLoop(dir:"down") and #readLoop(dir:"up") at once so the core sees both
   *  lanes (which it reduces to RunResult.bidirectional.{down,up}). */
  #openBidirectional(kind: TransportKind): void {
    void kind;
    this.#readLoop(null, "down");
    this.#readLoop(null, "up");
    throw NOT_IMPL("openBidirectional");
  }

  /**
   * Per-stream read/measure loop template. Accumulate bytes over a ~60ms
   * wall-window, then push host.ingestThroughput(dir, bytesPerSec, bytes).
   *   • On a RECOVERABLE stream error (transient drop): host.stall({reason:
   *     "connection-lost", transport: this.#activeTransport, detail}) — the core
   *     freezes measured-time — then attempt to re-open; on success host.resume()
   *     (or just let the next real sample auto-resume via the watchdog).
   *   • On an UNRECOVERABLE error: host.fail("connection-lost", message, cause).
   * Push NOTHING during the dead air — only real bytes become samples
   * (principle 1).
   */
  #readLoop(reader: unknown, dir: FlowDirection): void {
    void reader;
    void dir;
    // On a recoverable drop, the stall carries the transport that dropped:
    //   this.#host!.stall({ reason: "connection-lost", transport: this.#activeTransport ?? undefined, detail });
    void this.#activeTransport;
    throw NOT_IMPL("readLoop");
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
    if (this.#abort) {
      this.#abort.abort();
      this.#abort = null;
    }
    this.#activeTransport = null;
  }
}
