/* ============================================================
 * The Graphite Meter — Runner Contract (§2.1)
 * Types only. The UI is engine-agnostic: it consumes events
 * from any object implementing `NetworkRunner`.
 * ============================================================ */

/* ---------- Lifecycle ---------- */
/* Phase sequence: every enabled stage is preceded by its own self-contained
 * `warmup` window, e.g. all stages on →
 *   idle → warmup → latency → warmup → download → warmup → upload → complete
 * A warmup is omitted when its stage is off or `duration.warmupMs <= 0`. There
 * is no standalone global warmup: each warmup primes only the stage that
 * follows it, so stages carry no cross-dependencies. See the warmup-contract
 * note below `RunnerConfig`. */
export type Phase =
  | "idle"
  | "warmup"
  | "latency"
  | "download"
  | "upload"
  | "bidirectional" // concurrent down+up load phase (backend-mostly; UI deferred)
  | "complete"
  | "aborted"
  | "error";

/** Which way bytes are flowing for a throughput sample. Travels WITH the sample
 *  so the core never infers direction from the phase — that lets a single
 *  `bidirectional` phase carry concurrent down+up samples unambiguously. */
export type FlowDirection = "down" | "up";

export type ConnectivityState =
  | "connected"
  | "degraded" // jitter / minor packet loss
  | "unstable" // significant loss
  | "offline";

/* ---------- Overhead compensation (§13.3) ---------- */
/** Estimates true wire-rate from measured browser throughput. Each factor is
 *  independently toggleable; the numeric params feed the protocol accounting.
 *  Carried here as of Batch A but inert until the estimator lands in Batch C —
 *  `enabled: false` (or all factors off) yields a 1.0 multiplier. */
export interface OverheadCompensationConfig {
  enabled: boolean;
  factors: {
    ethernetFraming: boolean; // IP + L4 + Ethernet preamble/FCS/VLAN
    tlsRecords: boolean; // TLS record header + AEAD tag (TCP/TLS)
    applicationFraming: boolean; // HTTP/2 DATA, HTTP/3 QUIC, WS masks
    reversePathControl: boolean; // TCP ACKs / QUIC control traffic
    lossRetransmission: boolean; // retransmit tax (capped by maxLossRatio)
    steadyStateRamp: boolean; // lift toward late-test plateau
    browserRuntime: boolean; // GC/scheduling/render variance tax
  };
  params: {
    mtuBytes: number; // path MTU (1500 typical)
    vlanTagged: boolean; // 802.1Q tag adds 4B per frame
    tcpOptionsBytes: number; // timestamps/SACK (~12)
    framePayloadBytes: number; // HTTP/2 DATA frame payload window
    tlsRecordBytes: number; // TLS record header (5)
    aeadTagBytes: number; // AEAD auth tag (16)
    quicConnIdBytes: number; // QUIC connection-id length (8)
    maxLossRatio: number; // cap on the loss/retransmission factor (0–1)
  };
}

/* ---------- Adaptive duration (§13.4) ---------- */
/** Confidence-based early phase exit. `enabled: false` by default so fixed
 *  durations behave exactly as the base build until Batch D wires it. */
export interface AdaptiveDurationConfig {
  enabled: boolean;
  minCoverageRatio: number; // require ≥ this fraction of nominal duration first
  stabilityThreshold: number; // 0–1 stability-score gate to exit early
  maxPhaseReductionRatio: number; // never cut a phase by more than this fraction
  minLatencySamples: number; // floor before a latency phase may exit
  minTransferSamples: number; // floor before a transfer phase may exit
  glideMs: number; // real-time duration of the early-finish acceleration glide
}

/* ---------- Live measurement stability (§13.4) ---------- */
/** Coarse band of the 0–1 stability score, surfaced as the result-card pip. */
export type StabilityBand = "low" | "medium" | "high";

/** Live stability snapshot for a measured phase — the single signal the pip,
 *  the early-finish glide, and the result selection all read. */
export interface StabilitySnapshot {
  phase: Extract<Phase, "latency" | "download" | "upload">;
  score: number; // 0–1 stability score (adaptive.ts)
  band: StabilityBand;
  sampleCount: number; // usable samples in the confidence window
}

/* ---------- Configuration passed INTO the runner ---------- */
export interface RunnerConfig {
  /** Enabled measured stages. `bidirectional` (concurrent down+up) defaults
   *  off; when on it runs after upload with its own warmup. */
  stages: { latency: boolean; download: boolean; upload: boolean; bidirectional: boolean };
  /** When the latency stage is off, also skip the under-load latency pings
   *  taken during download/upload — so latency is fully off (no measurement,
   *  no profile, no chart line) rather than just dropping the idle phase. */
  skipLoadedLatencyWhenStageOff: boolean;
  /** Per-phase durations. Every measured `*Ms` is a TEST-TIME BUDGET (§4) — the
   *  phase runs until that much VALID measurement time is consumed, so dead air
   *  pushes its wall-clock end out. `warmupMs` stays plain wall-clock priming. */
  duration: {
    warmupMs: number;
    latencyMs: number;
    downloadMs: number;
    uploadMs: number;
    bidirectionalMs: number;
  };
  transport: {
    transfer: "webtransport" | "xhr-stream";
    latency: "webtransport" | "websocket";
  };
  pingConcurrency: "instant" | "medium" | "slow"; // → interval map
  parallelStreams: number; // 1–16
  endpoint: { host: string; port: number; path: string };
  /** Wire-rate estimation (§13.3). */
  compensation: OverheadCompensationConfig;
  /** Confidence-based early exit (§13.4). */
  adaptive: AdaptiveDurationConfig;
  /** Manual Y-axis ceiling for the gauge/chart; "auto" lets it self-scale. */
  visualization: { throughputMaxBytesPerSec: number | "auto" };
}

/* ---------- Raw samples emitted DURING a run ---------- */
export interface ThroughputSample {
  t: number; // ms since run start (monotonic)
  bytesPerSec: number; // instantaneous bytes/sec (raw, browser-native; UI converts/labels)
  bytesCumulative: number;
  streamCount: number;
  dir: FlowDirection; // which way these bytes flowed (down in download, up in upload, either in bidirectional)
  // The phase that produced this sample, stamped at ingest. Travels WITH the
  // sample (like `dir`) so consumers attribute it by tag — they never re-derive
  // the phase from timestamps. The single source of truth for sample→phase.
  phase: Extract<Phase, "download" | "upload" | "bidirectional">;
}

export interface LatencySample {
  t: number;
  rttMs: number;
  underLoad: boolean; // true if captured during dl/ul (bufferbloat)
  lost: boolean; // packet considered lost
  // The phase that produced this ping (like ThroughputSample.phase). Pre-test
  // probe pings carry "idle"; in-run pings carry their measured phase. Lets the
  // LatencyProfile bucket lanes by tag, never by re-derived time windows.
  phase: Phase;
}

export interface PhaseTransition {
  from: Phase;
  to: Phase;
  t: number;
}

/* ---------- Aggregate result (emitted on complete) ---------- */
export interface RunResult {
  download: ThroughputResult | null;
  upload: ThroughputResult | null;
  /** The bidirectional phase's two concurrent lanes, or null when the stage was
   *  off. Each lane reuses the same throughput reducer as download/upload. */
  bidirectional: { down: ThroughputResult; up: ThroughputResult } | null;
  latency: LatencyResult;
  bufferbloat: BufferbloatGrade;
  startedAt: number; // epoch ms
  durationMs: number;
}

/** How the headline value was derived: the trailing stable-plateau window
 *  (adaptive, still stable at finish) or the whole-phase average (adaptive off,
 *  or stability was lost before the phase ended). */
export type ResultMethod = "stable-window" | "full-average";

export interface ThroughputResult {
  meanBytesPerSec: number; // == reportedBytesPerSec — the headline value
  peakBytesPerSec: number;
  stabilityPct: number; // coefficient-of-variation based (0–100)
  totalBytes: number;
  reportedBytesPerSec: number; // headline (stable-window or full average)
  fullAverageBytesPerSec: number; // whole-phase mean, always (inspector/debug)
  method: ResultMethod;
  stabilityScore: number; // 0–1 stability at the moment the phase ended
  band: StabilityBand;
}

export interface LatencyResult {
  idleMs: number; // median unloaded (over the chosen window) — the headline
  minMs: number;
  p50Ms: number;
  p95Ms: number;
  jitterMs: number; // mean abs deviation
  packetLossPct: number;
  reportedMs: number; // == idleMs — the headline value, named for symmetry
  method: ResultMethod;
  stabilityScore: number;
  band: StabilityBand;
}

export interface BufferbloatGrade {
  grade: "A" | "B" | "C" | "D" | "F";
  idleMs: number;
  loadedMs: number;
  increaseMs: number; // loaded − idle
}

/* ---------- Structured termination ---------- */
/** Why a run ended abnormally. `user-abort` is modelled separately as the
 *  `"aborted"` phase (a deliberate stop, not a failure); every other reason is
 *  carried on the `error` event below.
 *
 *  Browser honesty: a server-initiated close and a network-level drop are
 *  generally indistinguishable from JS (both surface as a generic fetch
 *  TypeError), so they collapse into `connection-lost`. The distinctions a
 *  client CAN make reliably are reflected here: never reached the server
 *  (`preflight-failed`) vs lost mid-run (`connection-lost`), and a stall
 *  (`timeout`) vs a bad/unexpected response (`protocol-error`). */
export type TerminationReason =
  | "user-abort"
  | "preflight-failed" // handshake never reached / was rejected by a server
  | "connection-lost" // transport failed mid-run (server close or network loss)
  | "timeout" // a request/stream stalled past its deadline
  | "protocol-error" // malformed/unexpected response or close handshake
  | "internal-error" // a bug in the engine itself
  | "transport-unavailable"; // every negotiated transport failed to establish (§transport)

/* ---------- Transport negotiation (§transport) ---------- */
/** The connection method a backend may negotiate for a phase's I/O. A real
 *  engine tries these in preference order; each can fail to establish, and a
 *  failure of one is non-fatal as long as another succeeds. */
export type TransportKind = "webtransport" | "websocket" | "xhr-stream";

/** The stage a transport is being negotiated for. Mirrors schedule's StagePhase
 *  (re-declared here to keep contract.ts free of a schedule import — contract
 *  is the leaf types module) plus the priming `warmup`. */
export type TransportRole =
  | Extract<Phase, "latency" | "download" | "upload" | "bidirectional">
  | "warmup";

/** One step in negotiating a transport for a phase's I/O. A backend reports a
 *  `negotiating` attempt, then either `established` (success — measuring can
 *  begin) or `failed` (try the next kind). When every kind fails the backend
 *  raises a terminal `transport-unavailable` failure (see TerminationReason). */
export interface TransportAttempt {
  kind: TransportKind;
  role: TransportRole;
  status: "negotiating" | "established" | "failed";
  detail?: string;
}

/* ---------- Transient link health (§stall) ---------- */
/** A NON-terminal stall: the link went quiet mid-phase and the run is waiting
 *  to reconnect. Carried on the `stall` event; the core freezes measured-time
 *  accrual until a matching `resume`. `transport` (when known) names which
 *  connection dropped. This is NOT a failure — see `fail`/`RunnerError` for the
 *  terminal case (e.g. a stall that outlives MAX_STALL_MS becomes a
 *  `connection-lost` failure). */
export interface StallInfo {
  reason: TerminationReason;
  transport?: TransportKind;
  detail?: string;
}

/** A structured run failure, carried on the `error` event. Distinguishing a
 *  failure from a user abort (the `"aborted"` phase) and from a clean finish is
 *  the runner→webapp half of the lifecycle contract. */
export interface RunnerError {
  /** Failure category (never `user-abort` — that is the `"aborted"` phase). */
  reason: Exclude<TerminationReason, "user-abort">;
  /** Human-readable detail for logs / the toast. */
  message: string;
  /** The phase the run was in when it failed. */
  phase: Phase;
  /** Best-effort results from stages that completed before the failure, so the
   *  UI can still show what was measured. */
  partial?: {
    download: ThroughputResult | null;
    upload: ThroughputResult | null;
    latency: LatencyResult | null;
  };
  /** The original thrown value, for logging (not for display). */
  cause?: unknown;
}

/* ---------- Pre-test handshake info ---------- */
export interface InfraInfo {
  clientIp: string;
  server: { name: string; host: string; port: number; location?: string };
  preTestPingMs: number;
  engineVersion: string;
  protocolNegotiated: string;
}

/* ---------- Backend / server selection (future-proof seam) ----------
 *  One selectable measurement endpoint. A runner that fronts multiple backends
 *  may expose a list of these (via `listServers`) so a future UI can pick or
 *  auto-select the nearest; the chosen one becomes `InfraInfo.server`. Today
 *  every shipped runner targets a single endpoint (host "auto" lets it pick),
 *  so this is a contract seam only — no discovery or selection UI yet. */
export interface ServerCandidate {
  /** Stable id for selection/persistence. */
  id: string;
  name: string;
  host: string;
  port: number;
  path: string;
  location?: string;
  /** Last measured/advertised RTT, when known, for ranking. */
  rttMs?: number;
}

/* ---------- The event union the UI listens to ---------- */
export type RunnerEvent =
  | { type: "infra"; info: InfraInfo }
  | { type: "phase"; transition: PhaseTransition }
  | { type: "throughput"; sample: ThroughputSample }
  | { type: "latency"; sample: LatencySample }
  // Reserved seam: a backend MAY push an explicit connectivity state. No shipped
  // runner emits it yet — the store derives `effectiveConnectivity` from
  // loss/jitter/measuring, so this is purely an optional override for a real
  // engine that has a better signal.
  | { type: "connectivity"; state: ConnectivityState }
  // Progress within the active phase. `fraction` is 0–1 of the phase's
  // TEST-TIME budget consumed; the *Ms fields expose the raw measured-time
  // accrual so the UI can show a real "time remaining" (budget − elapsed) that
  // STOPS shrinking while stalled. `measuring` is the core's measured-time gate
  // — false while a stall (explicit or watchdog) has frozen accrual; the
  // grind-to-zero presentation (principle 2) keys off this, not off any sample.
  | {
      type: "progress";
      phase: Phase;
      fraction: number; // 0–1 within phase (of the test-time budget)
      phaseElapsedMs: number; // measured test-time consumed in this phase
      phaseBudgetMs: number; // this phase's test-time budget
      measuring: boolean; // false ⇒ accrual frozen (stalled)
    }
  | { type: "stability"; snapshot: StabilitySnapshot } // live measurement stability
  // Transient link health (NON-terminal): the run continues, hoping to
  // reconnect. `stall` freezes measured-time accrual (the phase end recedes by
  // the dead-air duration); `resume` un-freezes it. These drive the UI's
  // grind-to-zero + "connection lost" message — they carry NO sample and NEVER
  // enter the accumulator (principle 1).
  | { type: "stall"; info: StallInfo }
  | { type: "resume" }
  // Transport negotiation telemetry: which connection method is being tried for
  // a phase, and whether it's negotiating / established / failed. Re-emitted
  // verbatim by the core; the store records it (UI surface deferred — §9).
  | { type: "transport"; attempt: TransportAttempt }
  // Per-stage final result, emitted the instant each measured phase ends — so a
  // finished stage shows its real result while later stages still run. Stages
  // are independent: each carries its own headline/method/band (§13.4).
  | { type: "stageResult"; stage: "download" | "upload"; result: ThroughputResult }
  | { type: "stageResult"; stage: "latency"; result: LatencyResult }
  | { type: "complete"; result: RunResult }
  // Abnormal end (not user-abort, which is the "aborted" phase). Structured so
  // the UI can tell preflight-unreachable from a mid-run drop and surface any
  // partial results — see RunnerError.
  | { type: "error"; error: RunnerError };

/* ---------- Runtime anomaly injection (§13.6 — Developer panel) ---------- */
/** A live, dev-only perturbation fired into a *running* engine. Unlike the
 *  construction-time `DummyOptions.anomalies` (phase fractions), these fire
 *  relative to the current moment in the active phase — the Workbench
 *  Developer panel triggers them via `wire.injectAnomaly`. */
export type RunnerAnomaly =
  | { kind: "latency-spike"; magnitude?: number; durationMs?: number } // rtt ×magnitude
  | { kind: "packet-loss"; magnitude?: number; durationMs?: number } // loss probability
  | { kind: "throughput-drop"; magnitude?: number; durationMs?: number } // bytesPerSec ×(1−magnitude)
  // A full connection drop (dead air): the backend host.stall()s now and
  // host.resume()s after durationMs. Makes the whole stall/grind-to-zero
  // scenario visually testable with the dummy (§drop UX).
  | { kind: "connection-drop"; durationMs?: number };

/* ---------- The contract ---------- */
export interface NetworkRunner {
  start(config: RunnerConfig): void;
  abort(): void;
  /** Pre-test handshake; resolves InfraInfo. Pings every `intervalMs`. */
  probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo>;
  on(handler: (e: RunnerEvent) => void): () => void; // returns unsubscribe
  /** Apply a live change to the enabled stage set mid-run — only future
   *  (not-yet-started) stages are affected, so toggling one off shortens the
   *  remaining run. OPTIONAL so a minimal engine can omit live reconfigure;
   *  no-op when idle. */
  reconfigureStages?(stages: RunnerConfig["stages"]): void;
  /** OPTIONAL — list the measurement endpoints this runner can target, for a
   *  future server-selection UI. Single-backend runners omit it. */
  listServers?(): Promise<ServerCandidate[]>;
  /** OPTIONAL — fire a live anomaly into an in-flight run (§13.6). Kept
   *  optional so a minimal real engine need not implement it. */
  injectAnomaly?(a: RunnerAnomaly): void;
  readonly phase: Phase;
}

/* ---------- Warmup contract ----------
 *  Each enabled stage is preceded by exactly one `"warmup"` window of
 *  `duration.warmupMs` (omitted when that is <= 0). During the window the runner
 *  primes the connection(s) the *following* stage needs, concurrently:
 *    • latency  → the latency (ping) connection
 *    • download → the download transfer connection, plus — when loaded-latency
 *                 is active — the latency connection too (same window)
 *    • upload   → the upload transfer connection, plus the latency connection
 *                 when loaded-latency is active
 *  The window is always emitted to the UI as the generic `"warmup"` phase; which
 *  stage it primes is backend-only (the dummy records it as `warmupFor`). The
 *  single `warmupMs` setting governs every stage's warmup, so the runner timeline
 *  and the UI ETA stay in agreement. */
