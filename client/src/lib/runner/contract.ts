// Shared runner contract: phases, config, events, result shapes, and backend
// interfaces used by both the UI and measurement engines.

import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../api/preflight";

/* ---------- Lifecycle ---------- */
/* Phase sequence: every enabled stage is preceded by its own self-contained
 * `warmup` window, e.g. all stages on →
 *   idle → connecting → warmup → latency → warmup → download → warmup → upload → complete
 * A warmup is omitted when its stage is off or `duration.warmupMs <= 0`. There
 * is no standalone global warmup: each warmup primes only the stage that
 * follows it, so stages carry no cross-dependencies. See the warmup-contract
 * note below `RunnerConfig`. */
export type Phase =
  | "idle"
  | "connecting"
  | "warmup"
  | "latency"
  | "download"
  | "upload"
  | "bidirectional" // concurrent down+up load phase (its own warmup; combined-rate stability)
  | "complete"
  | "aborted"
  | "error";

/** Which way bytes are flowing for a throughput sample. Travels WITH the sample
 *  so the core never infers direction from the phase — that lets a single
 *  `bidirectional` phase carry concurrent down+up samples unambiguously. */
export type FlowDirection = "down" | "up";
export type ProtocolTarget = "http1" | "http2" | "http3";
export type ConnectionRole = "throughput" | "latency";
/** Advertised transfer target id; "current" resolves from the discovery hop. */
export type ThroughputTargetSelection = string;
export type PingCadence = "instant" | "medium" | "slow";

/* ---------- Phase activity descriptor (core → backend) ----------
 *  The self-contained description of WHAT a stage exercises, resolved ONCE by
 *  the scheduler from `RunnerConfig` and handed to the backend on every stage
 *  lifecycle hook (onStageBegin/onStageMeasure/onStageEnd in core.ts). The
 *  backend reads NOTHING from global config to decide which connections to open:
 *  `transfer` names the byte lanes and `loadedLatency` says whether concurrent
 *  pings run. A stage's warmup window and its measured window carry the SAME
 *  activity object, so the connection a warmup primes is the exact one the
 *  measurement reuses. This makes every combination explicit — latency-only,
 *  download, upload, bidirectional, and each transfer variant with or without
 *  loaded latency — without the backend inferring anything. */
export interface PhaseActivity {
  /** The measured stage this activity belongs to. */
  stage: Extract<Phase, "latency" | "download" | "upload" | "bidirectional">;
  /** Byte lanes to open: `[]` (latency-only), `["down"]`, `["up"]`, or both. */
  transfer: FlowDirection[];
  /** Run concurrent pings during the measured window (loaded latency /
   *  bufferbloat). Always false for the latency stage (which measures IDLE
   *  latency); for transfer stages it folds in the "skip loaded latency when the
   *  latency stage is off" config rule, resolved by the scheduler. */
  loadedLatency: boolean;
}

export type ConnectivityState =
  | "connected"
  | "degraded" // jitter / minor packet loss
  | "unstable" // significant loss
  | "offline";

/* ---------- Overhead compensation ---------- */
/** Estimates forward-direction physical link occupancy from application bytes. */
/** Path the transfer takes — picks which overheads physically apply. Driven by
 *  the UI "Connection profile" preset; loopback has no link layer, a tunnel adds
 *  outer encapsulation, etc. (see applyConnectionProfile in compensation.ts). */
export type ConnectionProfile = "lan" | "loopback" | "tunnel" | "custom";

/** Browser-facing wire transport, detected from Resource Timing or overridden. */
export type CompensationTransport =
  | "http1-clear" // HTTP/1.1, no TLS
  | "https-tls" // HTTP/1.1 over TLS
  | "http2" // HTTP/2 over TLS (DATA framing)
  | "http3-quic"; // HTTP/3 over QUIC (UDP)

export type CompensationTransportSetting = "auto" | CompensationTransport;
export type CompensationIPVersionSetting = "auto" | 4 | 6;

export interface OverheadCompensationConfig {
  /** Physical first-hop preset. The browser-facing HTTP transport is detected. */
  profile: ConnectionProfile;
  transport: CompensationTransportSetting;
  params: {
    mtuBytes: number;
    ipVersion: CompensationIPVersionSetting;
    vlanTagged: boolean; // 802.1Q tag adds 4B per frame
    tcpOptionsMinBytes: number;
    tcpOptionsMaxBytes: number;
    encapsulationBytes: number;
    quicConnIdMinBytes: number;
    quicConnIdMaxBytes: number;
  };
}

/* ---------- Adaptive duration ---------- */
/** Confidence-based early phase exit. `enabled: false` by default, preserving
 *  fixed-duration behavior. */
export interface AdaptiveDurationConfig {
  enabled: boolean;
  minCoverageRatio: number; // require ≥ this fraction of nominal duration first
  stabilityThreshold: number; // 0–1 stability-score gate to exit early
  maxPhaseReductionRatio: number; // never cut a phase by more than this fraction
  minLatencySamples: number; // floor before a latency phase may exit
  minTransferSamples: number; // floor before a transfer phase may exit
  glideMs: number; // real-time duration of the early-finish acceleration glide
}

/* ---------- Live measurement stability ---------- */
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

export interface TransferStreamPolicy {
  mode: "auto" | "forced";
  /** H1 per-direction ceiling in auto mode; exact count in forced mode. */
  count: number;
}

/* ---------- Configuration passed INTO the runner ---------- */
export interface RunnerConfig {
  /** Enabled measured stages. `bidirectional` (concurrent down+up) defaults
   *  off; when on it runs after upload with its own warmup. */
  stages: {
    latency: boolean;
    download: boolean;
    upload: boolean;
    bidirectional: boolean;
  };
  /** When the latency stage is off, also skip the under-load latency pings
   *  taken during download/upload — so latency is fully off (no measurement,
   *  no profile, no chart line) rather than just dropping the idle phase. */
  skipLoadedLatencyWhenStageOff: boolean;
  /** Per-phase wall-time budgets. A stage at its boundary waits for an active
   *  connection or fails at max-stall; warmup is unmeasured priming. */
  duration: {
    warmupMs: number;
    latencyMs: number;
    downloadMs: number;
    uploadMs: number;
    bidirectionalMs: number;
  };
  /** PING wire cadence for the unloaded latency stage, including warmup. */
  pingCadence: PingCadence;
  /** PING wire cadence during transfer stages, including warmup. */
  loadedPingCadence: PingCadence;
  transferStreams: TransferStreamPolicy;
  /** Experimental: request adaptively-sized download chunks instead of one long
   *  stream per lane (A/B ramp responsiveness on real lines). Default off. */
  experimentalChunkedDownload: boolean;
  /** Independently selected throughput and latency targets. */
  transports: {
    throughputTarget: ThroughputTargetSelection;
    latencyTarget: "auto" | string;
  };
  /** Wire-rate estimation. */
  compensation: OverheadCompensationConfig;
  /** Confidence-based early exit. */
  adaptive: AdaptiveDurationConfig;
  /** Manual Y-axis ceiling for the gauge/chart; "auto" lets it self-scale. */
  visualization: { throughputMaxBytesPerSec: number | "auto" };
}

/* ---------- Raw samples emitted DURING a run ---------- */
export interface ThroughputSample {
  t: number; // ms since run start (monotonic)
  bytesPerSec: number; // smoothed live rate; exact results use private byte/time observations
  bytesCumulative: number;
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
  stage: TransportRole | null;
  t: number; // exact boundary on the run's measured timeline
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

/** How a headline was derived. A stable window begins when adaptive completion
 *  arms and is used only if stability holds until the phase ends. */
export type ResultMethod = "stable-window" | "full-average";

export interface ThroughputResult {
  meanBytesPerSec: number; // == reportedBytesPerSec — the headline value
  peakBytesPerSec: number;
  stabilityPct: number; // coefficient-of-variation based (0–100)
  totalBytes: number;
  reportedBytesPerSec: number; // effective bytes / represented time
  fullAverageBytesPerSec: number; // same effective whole-window rate
  method: ResultMethod;
  stabilityScore: number; // 0–1 stability at the moment the phase ended
  band: StabilityBand;
  /** Under-load ping timeout percentage; a quality signal, not TCP packet loss. */
  packetLossPct: number;
  /** True when bytes and time came from the server upload receiver. */
  serverAuthoritative?: boolean;
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
  | "transport-unavailable"; // every negotiated transport failed to establish

/* ---------- Transport negotiation ---------- */
/** The connection method a backend may negotiate for a phase's I/O. A real
 *  engine tries these in preference order; each can fail to establish, and a
 *  failure of one is non-fatal as long as another succeeds. */
export type TransportKind = "webtransport" | "websocket" | "fetch-stream";

/** The stage a transport is being negotiated for. A backend negotiates a
 *  stage's transport ONCE, at stage begin — the warmup primes it and the
 *  measured window reuses it — so there is no separate priming `warmup` role.
 *  Mirrors schedule's StagePhase (re-declared here to keep contract.ts free of a
 *  schedule import — contract is the leaf types module). */
export type TransportRole = Extract<
  Phase,
  "latency" | "download" | "upload" | "bidirectional"
>;

/** A stage that could not run: the server lacks the capability, no transport
 *  could be negotiated, or the connection never established. NON-terminal —
 *  the run continues with the remaining stages; the UI explains the gap in
 *  that stage's instrument (gauge for transfers, profile for latency). */
export interface StageFailure {
  stage: TransportRole;
  reason: Exclude<TerminationReason, "user-abort">;
  message: string;
}

/** One step in negotiating a transport for a phase's I/O. A backend reports a
 *  `negotiating` attempt, then either `established` (success — measuring can
 *  begin) or `failed` (try the next kind). When every kind fails the backend
 *  skips the stage via failStage (or fails the run when nothing else can run). */
export interface TransportAttempt {
  kind: TransportKind;
  role: TransportRole;
  status: "negotiating" | "established" | "failed";
  detail?: string;
}

/* ---------- Transient link health ---------- */
/** A NON-terminal stall: the link went quiet mid-phase and the run is waiting
 *  to reconnect. Elapsed time continues so the gap affects throughput.
 *  `transport` (when known) names which
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

/* ---------- Engine identity & capabilities ----------
 *  Static self-description of a runner backend. The long-term model is ONE real
 *  engine that can drive many transports, with the user picking per role from
 *  these lists — so capabilities live on the ENGINE, not one runner per
 *  protocol. The roles differ: latency runs on a message bus (websocket,
 *  webtransport datagrams), throughput on byte lanes (fetch streams over
 *  h1.1/h2/h3, webtransport streams) — websocket is never a throughput
 *  transport. No selection UI yet; the Endpoint info renders the lists so the
 *  seam is visible. */
export interface EngineInfo {
  /** Engine id, e.g. "real" | "dummy". */
  name: string;
  /** Per-engine version. Both built-ins are versioned with the client build
   *  today (same build phase); the field exists so a future pluggable engine
   *  can version independently. */
  version: string;
  /** Transports this engine can drive for latency probing, preference order. */
  latencyTransports: string[];
  /** Transports this engine can drive for throughput transfer, preference order. */
  throughputTransports: string[];
}

/* ---------- Pre-test handshake info ---------- */
export interface InfraInfo {
  clientIp: string;
  clientIpVersion: 4 | 6;
  clientIpSource: "socket" | "forwarded";
  /** Independent H1/WebSocket latency path; it may select another address family. */
  latencyClientIp?: string;
  latencyClientIpVersion?: 4 | 6;
  latencyClientIpSource?: "socket" | "forwarded";
  server: { name: string; host: string; port: number; location?: string };
  preTestPingMs: number;
  engineVersion: string;
  discoveryGeneration: string;
  protocolNegotiated: string;
  selectedThroughputTarget?: string;
  selectedThroughputProtocol?: ProtocolTarget;
  selectedLatencyTarget?: string;
  selectedLatencyTransport?: TransportKind;
  verifiedLatencyProtocol?: string;
  latencyProtocolNegotiated?: string;
  /** Browser-facing protocol from Resource Timing (e.g. http/1.1, h2, h3). */
  firstHopProtocol?: string;
  firstHopSecure?: boolean;
}

export type TransportDiscoveryState =
  "advertised" | "browser-blocked" | "not-advertised";

export interface DiscoveredTarget<T> {
  state: TransportDiscoveryState;
  target?: T;
}

/** Server-advertised transports classified against the page that will use
 * them. Emitted as soon as /preflight completes, before selection or probing. */
export interface TransportDiscovery {
  generation: string;
  engineVersion: string;
  server: { name: string; host: string; port: number; location?: string };
  fetchedAt: number;
  pageOrigin: string;
  pageSecure: boolean;
  pageProtocol?: string;
  throughput: Record<string, DiscoveredTarget<FetchThroughputTarget>>;
  latency: Record<string, DiscoveredTarget<WebSocketLatencyTarget>>;
}

/* ---------- The event union the UI listens to ---------- */
export type RunnerEvent =
  | { type: "transportDiscovery"; discovery: TransportDiscovery }
  | { type: "infra"; info: InfraInfo }
  | { type: "phase"; transition: PhaseTransition }
  | { type: "throughput"; sample: ThroughputSample }
  | { type: "latency"; sample: LatencySample }
  // Reserved seam: a backend MAY push an explicit connectivity state. No shipped
  // runner emits it yet — the store derives `effectiveConnectivity` from
  // loss/jitter/measuring, so this is purely an optional override for a real
  // engine that has a better signal.
  | { type: "connectivity"; state: ConnectivityState }
  // Progress within the active wall-time budget. `measuring` is false while
  // delivery is stalled; the grind-to-zero presentation keys off this flag.
  | {
      type: "progress";
      phase: Phase;
      fraction: number; // 0–1 within the phase budget
      phaseElapsedMs: number;
      phaseBudgetMs: number;
      measuring: boolean; // false while delivery is stalled
    }
  | { type: "stability"; snapshot: StabilitySnapshot } // live measurement stability
  // Transient link health (NON-terminal): the run continues, hoping to
  // reconnect. Time still contributes to effective throughput. These drive the
  // UI's grind-to-zero + "connection lost" message.
  | { type: "stall"; info: StallInfo }
  | { type: "resume" }
  // Transport negotiation telemetry: which connection method is being tried for
  // a phase, and whether it's negotiating / established / failed. Re-emitted
  // verbatim by the core; the store records it (UI surface deferred).
  | { type: "transport"; attempt: TransportAttempt }
  // A stage was skipped because it couldn't run (see StageFailure). The rest of
  // the run continues; the UI surfaces the reason in the stage's instrument.
  | { type: "stageSkipped"; failure: StageFailure }
  // Per-stage final result, emitted the instant each measured phase ends — so a
  // finished stage shows its real result while later stages still run. Stages
  // are independent: each carries its own headline/method/band.
  | {
      type: "stageResult";
      stage: "download" | "upload";
      result: ThroughputResult;
    }
  | { type: "stageResult"; stage: "latency"; result: LatencyResult }
  | { type: "complete"; result: RunResult }
  // Abnormal end (not user-abort, which is the "aborted" phase). Structured so
  // the UI can tell preflight-unreachable from a mid-run drop and surface any
  // partial results — see RunnerError.
  | { type: "error"; error: RunnerError };

/* ---------- Runtime anomaly injection — Developer panel ---------- */
/** A live, dev-only perturbation fired into a *running* engine. Unlike the
 *  construction-time `DummyOptions.anomalies` (phase fractions), these fire
 *  relative to the current moment in the active phase — the Settings
 *  Developer panel triggers them via `wire.injectAnomaly`. */
export type RunnerAnomaly =
  | { kind: "latency-spike"; magnitude?: number; durationMs?: number } // rtt ×magnitude
  | { kind: "packet-loss"; magnitude?: number; durationMs?: number } // loss probability
  | { kind: "throughput-drop"; magnitude?: number; durationMs?: number } // bytesPerSec ×(1−magnitude)
  // A full connection drop (dead air): the backend host.stall()s now and
  // host.resume()s after durationMs. Makes the whole stall/grind-to-zero
  // scenario visually testable with the dummy.
  | { kind: "connection-drop"; durationMs?: number };

/** Settings the core can safely apply after a run has started. Connection and
 * worker construction remain fixed; these only reshape the remaining timeline
 * or its completion rule. */
export type LiveRunConfig = Pick<
  RunnerConfig,
  "stages" | "duration" | "adaptive"
>;

/* ---------- The contract ---------- */
export interface NetworkRunner {
  /** Verify the selected target, then run. Emits `connecting` immediately so
   *  asynchronous path verification is visible and cancellable. */
  start(config: RunnerConfig, prepared?: InfraInfo): Promise<void>;
  abort(): void;
  /** Pre-test handshake; resolves InfraInfo. Pings every `intervalMs`. */
  probe(
    config: RunnerConfig,
    signal?: AbortSignal,
    role?: ConnectionRole,
  ): Promise<InfraInfo>;
  /** Static engine identity + transport capabilities (no I/O). */
  describe(): EngineInfo;
  on(handler: (e: RunnerEvent) => void): () => void; // returns unsubscribe
  /** Apply settings that are safe to change during a run. */
  reconfigure?(config: LiveRunConfig): void;
  /** OPTIONAL — fire a live anomaly into an in-flight run. Kept
   *  optional so a minimal real engine need not implement it. */
  injectAnomaly?(a: RunnerAnomaly): void;
  readonly phase: Phase;
}

/* ---------- Stage lifecycle & warmup contract ----------
 *  Connections are owned by the STAGE, not by the phase label. The core drives a
 *  three-call lifecycle per enabled stage (see `RunnerBackend` in core.ts), each
 *  call carrying the stage's resolved {@link PhaseActivity}:
 *    onStageBegin(activity)   — open + PRIME every connection the activity names;
 *                               asynchronous preparation pauses the stage clock
 *                               (the `transfer` lanes, plus the ping channel when
 *                               `loadedLatency` or a latency stage). Fires at the
 *                               start of the stage's warmup window. No measuring.
 *    onStageMeasure(activity) — the warmup window has elapsed; START measuring on
 *                               the SAME primed connections (never reopen). Fires
 *                               immediately after onStageBegin when warmupMs<=0.
 *    onStageEnd(activity)     — the measured window ended (boundary, early finish,
 *                               or run end); close the stage's connection(s).
 *  The configured warmup begins only after asynchronous preparation resolves,
 *  so it remains a minimum wire-warming interval rather than a deadline that
 *  setup can consume. Because begin/measure/end bracket ONE connection set, the
 *  warmup genuinely warms the wire the measurement runs over — there is no cold reconnect at the
 *  warmup→measure seam (the point of a warmup). Each enabled stage is
 *  still preceded by exactly one `"warmup"` window of `duration.warmupMs`
 *  (omitted when <= 0), emitted to the UI as the generic `"warmup"` phase; the
 *  stage split is backend-only. The single `warmupMs` setting governs every
 *  stage's warmup, so the runner timeline and the UI ETA stay in agreement. */
