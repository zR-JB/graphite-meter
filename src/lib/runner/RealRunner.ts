/* ============================================================
 * The Graphite Meter — Real Backend SKELETON (§14.4)
 * ============================================================
 *
 * A compile-clean stub implementing `RunnerBackend` (core.ts) so a
 * future engine talking to live speedtest-server APIs can be filled
 * in WITHOUT touching the core, the UI, or the store. The shared
 * `RunnerCore` already owns the timeline, sequencing, accumulation,
 * stability, early-stop, result reduction, and the whole event
 * stream — so a real engine implements ONLY network I/O.
 *
 * ── What the backend is responsible for ─────────────────────
 *  • probe()         — the preflight handshake (+ a few pre-test pings).
 *  • connection lifecycle — open/prime on onPhaseEnter, close on
 *                    onPhaseExit / onAbort / onComplete. The CORE decides
 *                    WHEN phases change; the backend just reacts.
 *  • measurement     — stream/measure raw throughput + latency and push
 *                    them in via host.ingestThroughput / host.ingestLatency.
 *                    (Push model: do NOT implement onTick — that is the
 *                    dummy's pull hook. Real samples arrive as bytes do.)
 *  • failures        — report drops/timeouts/protocol errors with
 *                    host.fail(reason, message, cause). User abort is the
 *                    core's abort()/"aborted" phase, NOT a failure.
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
  /** The core handle: push samples / emit / report failures through it. */
  #host: CoreHost | null = null;
  /** AbortController for in-flight fetches/streams; aborted in onAbort. */
  #abort: AbortController | null = null;

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
    throw NOT_IMPL("onRunStart");
  }

  /**
   * A phase has begun (the core decided the transition). Open/prime the
   * connection(s) it needs and begin measuring:
   *   • warmup    → prime the following stage's connection(s) (`warmupFor`);
   *                 for transfers, also the latency connection when loaded-
   *                 latency is active (see the warmup contract in contract.ts).
   *   • latency   → `WS {path}/ping` / WebTransport datagrams / `GET {path}/ping`
   *                 (per config.transport.latency). RTT = now − sent; unacked /
   *                 timed-out = lost. Push via host.ingestLatency(rtt, false, lost)
   *                 at the config.pingConcurrency interval.
   *   • download  → `GET {path}/download?bytes=N` × config.parallelStreams
   *                 (or WebTransport per config.transport.transfer). Sum received
   *                 bytes/sec; push host.ingestThroughput(bytesPerSec, bytesDelta)
   *                 ~16Hz. Reuse pings under load → host.ingestLatency(..., true, …).
   *                 Server streams INCOMPRESSIBLE random bytes.
   *   • upload    → `POST {path}/upload` streamed body × parallelStreams; server
   *                 discards (may echo a byte count); measure sent bytes/sec.
   */
  onPhaseEnter(phase: StagePhase | "warmup", warmupFor?: StagePhase): void {
    void phase;
    void warmupFor;
    throw NOT_IMPL("onPhaseEnter");
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
  }
}
