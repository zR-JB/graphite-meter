/* ============================================================
 * The Graphite Meter — Real Runner SKELETON (§14.4)
 * ============================================================
 *
 * A compile-clean stub that satisfies the `NetworkRunner` contract
 * (§2.1) so a future engine talking to live speedtest-server APIs can be
 * filled in WITHOUT touching the UI or the store. The DummyRunner remains
 * the shipped/default engine; this class is never instantiated by default.
 *
 * ── How to fill this in ─────────────────────────────────────
 *  1. Implement each method per the TARGET ENDPOINT comment above it
 *     (mirrors the backend API table in §14.4 / docs/REAL_RUNNER.md).
 *  2. Re-emit the EXACT same `RunnerEvent` stream the dummy emits — the UI
 *     and store are engine-agnostic and only consume those events.
 *  3. To go live, change ONE line in `wire.svelte.ts`:
 *         if (!runner) runner = new RealRunner({ ... });
 *     Nothing else in the app changes.
 *
 * ── Event / timing contract (must match DummyRunner) ────────
 *  • probe()  → resolves `InfraInfo`; MAY emit a few pre-test `latency`
 *               samples (underLoad:false, negative `t`) for the sparkline.
 *  • start()  → emits `phase` transitions — each enabled stage is preceded by
 *               its own `warmup` (idle→warmup→latency→warmup→download→warmup→
 *               upload→complete; a warmup is omitted when its stage is off or
 *               warmupMs<=0). During a stage's warmup the engine primes that
 *               stage's connection — for transfers, also the latency connection
 *               when loaded-latency is active (see the warmup contract in
 *               contract.ts). Plus `progress` (fraction 0–1 within phase),
 *               `throughput` samples at ~16Hz (every ~60ms, download/upload
 *               only), `latency` samples at the ping interval (latency phase
 *               + under-load during transfer), an optional `connectivity`
 *               state, and finally `complete` carrying a `RunResult`.
 *  • abort()  → stops work and emits a `phase` transition to "aborted".
 *
 * ── Units rule (§14.0 / §14.4) ──────────────────────────────
 *  Raw on the wire: the server serves/sinks BYTES; the client derives
 *  bytes/sec. `ThroughputSample.bytesPerSec` is raw instantaneous bytes/sec and
 *  `bytesCumulative` is raw bytes. NO bits, base-2/base-10, or unit conversion
 *  happens in the runner — that (incl. any bit/s rendering) is the UI's job.
 * ============================================================ */

import type {
  NetworkRunner,
  RunnerConfig,
  RunnerEvent,
  RunnerAnomaly,
  Phase,
  InfraInfo,
} from "./contract";

/** Construction options for a real engine. Mirrors what the dummy needs:
 *  the endpoint to target (host/port/path) plus anything preflight hands
 *  back (e.g. a session token). All optional so the class stays trivial to
 *  drop into `wire.svelte.ts`. */
export interface RealRunnerOptions {
  /** Backend base the API paths in §14.4 are relative to. Falls back to the
   *  `config.endpoint` passed into `start()` / `probe()` when omitted. */
  endpoint?: RunnerConfig["endpoint"];
  /** Optional bearer/session token from a prior preflight (§14.4 auth). */
  authToken?: string;
}

const NOT_IMPL = (method: string) =>
  new Error(`RealRunner.${method} not implemented — see docs/REAL_RUNNER.md`);

export class RealRunner implements NetworkRunner {
  /* ---------- Real plumbing (wired) ---------- */
  #handlers = new Set<(e: RunnerEvent) => void>();
  #phase: Phase = "idle";
  #opts: RealRunnerOptions;

  /** AbortController for in-flight fetches/streams; sockets close in abort(). */
  #abort: AbortController | null = null;

  constructor(opts: RealRunnerOptions = {}) {
    this.#opts = opts;
  }

  /** Engine-agnostic event fan-out — identical shape to DummyRunner.#emit. */
  #emit(e: RunnerEvent) {
    for (const h of this.#handlers) h(e);
  }

  /** Resolve the backend base for the API paths in §14.4: prefer the endpoint
   *  passed into start()/probe(), else the one supplied at construction. */
  #resolveEndpoint(
    passed?: RunnerConfig["endpoint"],
  ): RunnerConfig["endpoint"] | undefined {
    return passed ?? this.#opts.endpoint;
  }

  /** Subscribe to the RunnerEvent stream; returns an unsubscribe fn. */
  on(handler: (e: RunnerEvent) => void): () => void {
    this.#handlers.add(handler);
    return () => this.#handlers.delete(handler);
  }

  /** Current lifecycle phase — backed by a field flipped during a run. */
  get phase(): Phase {
    return this.#phase;
  }

  /* ================= PROBE ================= */
  /**
   * TARGET: `GET {endpoint.path}/preflight` (a.k.a. /config) — §14.4.
   * Obligation: resolve `InfraInfo` (client public IP, server identity,
   * negotiated protocol h2/h3/webtransport, engine version, pre-test ping).
   * May also `GET {path}/ping` (or `WS {path}/ping`) a few times to emit
   * idle-baseline `latency` samples for the pre-run sparkline.
   * Cross-cutting: CORS + Timing-Allow-Origin required for accurate timing.
   */
  async probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo> {
    // Effective base + auth the implementation will hit (kept referenced so the
    // plumbing is real and the skeleton typechecks).
    void this.#resolveEndpoint(endpoint);
    void this.#opts.authToken;
    throw NOT_IMPL("probe");
  }

  /* ================= START ================= */
  /**
   * Orchestrates the full run against the live backend, emitting the same
   * RunnerEvent stream + cadence as DummyRunner. Per §14.4:
   *   • latency phase  → `WS {path}/ping` (websocket) / WebTransport datagrams
   *                      / `GET {path}/ping` (HTTP fallback), chosen by
   *                      `config.transport.latency`. RTT = now − sent;
   *                      unacked/timed-out = packet loss. Emit `latency`
   *                      samples at the ping interval (config.pingConcurrency).
   *   • download phase → `GET {path}/download?bytes=N` × `config.parallelStreams`
   *                      concurrent streams (or WebTransport per
   *                      `config.transport.transfer`); sum received bytes/sec.
   *                      Emit `throughput` ~16Hz (~60ms). Server streams
   *                      INCOMPRESSIBLE random bytes; client derives bytesPerSec.
   *   • upload phase   → `POST {path}/upload` streamed body × parallelStreams;
   *                      server discards + may echo a received-byte count;
   *                      client measures sent bytes/sec → `throughput` ~16Hz.
   *   • Reuse pings under load for bufferbloat (underLoad:true).
   *   • Emit `phase` transitions + per-phase `progress`, then `complete`
   *     with the aggregated `RunResult`.
   * Open a fresh AbortController here so abort() can cancel everything.
   */
  start(config: RunnerConfig): void {
    // config.endpoint wins; fall back to the construction-time endpoint.
    void this.#resolveEndpoint(config.endpoint);
    throw NOT_IMPL("start");
  }

  /* ================= ABORT ================= */
  /**
   * TARGET: client-side only — §14.4. Cancel in-flight fetches/streams via
   * the AbortController and close any open sockets/WebTransport sessions,
   * then flip to "aborted" and emit the phase transition. Implemented here as
   * trivial plumbing so the seam behaves even before start() is wired.
   */
  abort(): void {
    if (this.#abort) {
      this.#abort.abort();
      this.#abort = null;
    }
    if (this.#phase === "idle" || this.#phase === "complete" || this.#phase === "aborted") {
      return;
    }
    const from = this.#phase;
    this.#phase = "aborted";
    this.#emit({ type: "phase", transition: { from, to: "aborted", t: 0 } });
  }

  /* ================= OPTIONAL: live anomaly (§13.6) ================= */
  /**
   * OPTIONAL on the contract — a minimal real engine need not implement it.
   * A real engine has no synthetic knob to perturb, so this dev-only hook is
   * a no-op here. Remove the method entirely to drop it from the surface.
   */
  injectAnomaly(_a: RunnerAnomaly): void {
    /* no-op: real transport has no synthetic anomaly to inject */
  }
}
