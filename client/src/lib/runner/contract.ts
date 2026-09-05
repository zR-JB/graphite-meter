// Shared runner contract for phases, config, events, results, and backend interfaces.

import type { Probe } from "../api/probe";
import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../api/endpoints";

/* ---------- Lifecycle ---------- */
/* Phase sequence, all stages on. */
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

/* Which way bytes flow for a throughput sample. */
export type FlowDirection = "down" | "up";
export type ProtocolTarget = "http1" | "http2" | "http3" | "negotiated";
export type ConnectionRole = "throughput" | "latency";
/** Advertised transfer target id; "current" resolves from the discovery hop. */
export type ThroughputTargetSelection = string;
export type PingCadence = "reply-driven" | "fast" | "medium" | "slow";

/* Warmup and measurement share one activity object, so preparation primes the connections measurement reuses. */
export interface PhaseActivity {
  /** The measured stage this activity belongs to. */
  stage: Extract<Phase, "latency" | "download" | "upload" | "bidirectional">;
  /** Byte lanes to open: `[]` (latency-only), `["down"]`, `["up"]`, or both. */
  transfer: FlowDirection[];
  /** Concurrent pings during transfer stages provide loaded-latency evidence. */
  loadedLatency: boolean;
}

export type ConnectivityState =
  | "connected"
  | "degraded" // RTT variation / occasional probe timeouts
  | "unstable" // frequent probe timeouts
  | "offline";

/* Automatic wire estimates ---------- Forward-direction physical link occupancy from application bytes. */

/** Browser-facing wire transport, detected from Resource Timing and security. */
export type CompensationTransport =
  | "http1-clear" // HTTP/1.1, no TLS
  | "https-tls" // HTTP/1.1 over TLS
  | "http2" // HTTP/2 over TLS (DATA framing)
  | "http3-quic"; // HTTP/3 over QUIC (UDP)

/* ---------- Adaptive duration ---------- */
/** Confidence-based early exit; disabled adaptive mode runs each phase for its full configured duration. */
export interface AdaptiveDurationConfig {
  enabled: boolean;
  minCoverageRatio: number; // require ≥ this fraction of nominal duration first
  stabilityThreshold: number; // stability-score gate (0..1) to exit early
  maxPhaseReductionRatio: number; // never cut a phase by more than this fraction
  minLatencySamples: number; // sample floor for a latency phase's early exit
  minTransferSamples: number; // sample floor for a transfer phase's early exit
  confirmationMs: number; // stability must remain eligible for this real interval
}

/* ---------- Live measurement stability ---------- */
/** Coarse band of the 0..1 stability score, surfaced as the result-card pip. */
export type StabilityBand = "low" | "medium" | "high";

/* Live stability snapshot drives the pip, revocable early-finish confirmation, and adaptive completion. */
export interface StabilitySnapshot {
  phase: Extract<Phase, "latency" | "download" | "upload" | "bidirectional">;
  score: number; // stability score 0..1 (adaptive.ts)
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
  /** Enabled measured stages. */
  stages: {
    latency: boolean;
    download: boolean;
    upload: boolean;
    bidirectional: boolean;
  };
  /** Skip transfer-stage latency pings when the standalone latency stage is off. */
  skipLoadedLatencyWhenStageOff: boolean;
  /** Per-phase wall-time budgets; warmup is unmeasured priming. */
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
  /** Lists the WebTransport datagram card; selection remains independent of this filter. */
  experimentalDatagramThroughput: boolean;
  /** Independently selected throughput and latency targets. */
  transports: {
    throughputTarget: ThroughputTargetSelection;
    latencyTarget: "auto" | string;
  };
  /** Confidence-based early exit. */
  adaptive: AdaptiveDurationConfig;
  /** Manual Y-axis ceiling for the gauge/chart; "auto" lets it self-scale. */
  visualization: { throughputMaxBytesPerSec: number | "auto" };
}

/* ---------- Raw samples emitted DURING a run ---------- */
/** Authoritative in-run latency outcome in the window realm's monotonic clock domain. */
export interface LatencyObservation {
  rttMs: number;
  /** Optional server application handling duration from this same reply. */
  reflectorHandlingMs?: number;
  lost: boolean;
  observedAtMs: number;
  /** A reply after the stage cutoff resolves its probe but is outside the RTT measurement window. */
  rttEligible?: boolean;
}

export interface ThroughputSample {
  t: number; // ms since run start (monotonic)
  bytesPerSec: number; // smoothed live rate; exact results use private byte/time observations
  bytesCumulative: number;
  dir: FlowDirection; // Direction travels with each sample; consumers do not infer it from the phase.
  phase: Extract<Phase, "download" | "upload" | "bidirectional">;
  /** Lines with different ids are intentionally discontinuous. */
  continuityId: number;
}

export interface LatencyBucket {
  t: number;
  startT: number;
  endT: number;
  medianRttMs: number | null;
  p95RttMs: number | null;
  maxRttMs: number | null;
  /** Exact consecutive-success RTT variation retained through aggregation. */
  firstRttMs: number | null;
  lastRttMs: number | null;
  rttDeltaSumMs: number;
  rttDeltaCount: number;
  pingCount: number;
  lossCount: number;
  underLoad: boolean; // True when captured during transfer load; phase carries the producer tag.
  phase: Phase;
  continuityId: number;
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
  /** The bidirectional phase's concurrent lanes, or null when that stage is off. */
  bidirectional: {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
  } | null;
  latency: LatencyResult | null;
  latencyByStage: Record<TransportRole, StageLatencySummary | null>;
  /** Unavailable unless both idle and loaded latency evidence exist. */
  bufferbloat: BufferbloatGrade | null;
  /** A usable result plus an entry here is a partial stage. */
  stageFailures: Partial<Record<TransportRole, StageFailure>>;
  startedAt: number; // epoch ms
  durationMs: number;
}

/** Throughput uses a stable plateau when adaptive completion is enabled, otherwise the full measured phase. */
type ResultMethod = "stable-window" | "full-average";

export interface ThroughputResult {
  meanBytesPerSec: number; // == reportedBytesPerSec, the headline value
  peakBytesPerSec: number;
  /** Fixed-time-bucket coefficient-of-variation descriptor (0..100). */
  stabilityPct: number;
  totalBytes: number;
  reportedBytesPerSec: number; // effective bytes / represented time
  fullAverageBytesPerSec: number; // same effective whole-window rate
  method: ResultMethod;
  stabilityScore: number; // stability (0..1) at the moment the phase ends
  band: StabilityBand;
  /** Under-load ping timeout percentage; a quality signal, not TCP packet loss. */
  probeTimeoutPct: number | null;
  /** True when bytes and time came from the server upload receiver. */
  serverAuthoritative?: boolean;
}

/** Diagnostic means over the same successful, in-window replies with valid
 * negotiated timing. Missing timing is omitted from this population only. */
export interface ReflectorTimingSummary {
  sampleCount: number;
  meanRawRttMs: number;
  meanHandlingMs: number;
  meanAdjustedRttMs: number;
}

/** Full measured stage; percentiles use nearest rank, with the midpoint median for P50. */
export interface StageLatencySummary {
  reflectorTiming?: ReflectorTimingSummary;
  /** False when a worker failure leaves the outcome population unknown. */
  accountingComplete: boolean;
  probeCount: number;
  timeoutCount: number;
  unresolvedCount: number;
  sendFailureCount: number;
  jitterPairs: number;
  minMs: number | null;
  maxMs: number | null;
  meanMs: number | null;
  p10Ms: number | null;
  p50Ms: number | null;
  p90Ms: number | null;
  p95Ms: number | null;
  jitterMs: number | null;
}

export interface LatencyResult {
  idleMs: number; // median unloaded over the chosen window, the headline
  minMs: number | null;
  p50Ms: number | null;
  p95Ms: number | null;
  jitterMs: number | null; // mean absolute consecutive-success RTT difference
  probeTimeoutPct: number | null;
  reportedMs: number; // == idleMs, the headline value, named for symmetry
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
/* `user-abort` is the `"aborted"` phase instead, a deliberate stop; every other reason rides the `error` event. */
export type TerminationReason =
  | "user-abort"
  | "preflight-failed" // the handshake never reaches, or a server rejects it
  | "connection-lost" // transport failed mid-run (server close or network loss)
  | "timeout" // a request/stream stalled past its deadline
  | "protocol-error" // malformed/unexpected response or close handshake
  | "internal-error" // a bug in the engine itself
  | "transport-unavailable"; // every negotiated transport failed to establish

/* ---------- Transport negotiation ---------- */
/* The connection method a backend may negotiate for a phase's I/O. */
export type TransportKind =
  "webtransport" | "webtransport-datagram" | "websocket" | "fetch-stream";

/** Warmup and measurement belong to the same stage and reuse its connections. */
export type TransportRole = Extract<
  Phase,
  "latency" | "download" | "upload" | "bidirectional"
>;

/** A failed stage retains any usable measurements and identifies the affected direction when known. */
export interface StageFailure {
  stage: TransportRole;
  /** The affected lane when a bidirectional stage keeps its other result. */
  direction?: FlowDirection;
  reason: Exclude<TerminationReason, "user-abort">;
  message: string;
}

/** Protocol evidence that determines how an upload stage may recover. */
export type RecoveryCause =
  | "transient-connection"
  | "unknown-upload-id"
  | "owner-mismatch"
  | "authentication-failure"
  | "capacity-refusal"
  | "protocol-refusal";

/* ---------- Transient link health ---------- */
/* A NON-terminal stall: the link is quiet mid-phase and the runner starts a bounded recovery lifecycle. */
export interface StallInfo {
  reason: TerminationReason;
  transport?: TransportKind; // the connection that dropped, when known
  detail?: string;
  /** Structural transport evidence; a generic disconnect stays transient. */
  recoveryCause?: RecoveryCause;
  /** The lane that first established this stage-wide stall. */
  direction?: FlowDirection;
}

/** Terminal failure is distinct from user cancellation and may retain usable partial measurements. */
export interface RunnerError {
  /** Failure category; `user-abort` is the `"aborted"` phase instead. */
  reason: Exclude<TerminationReason, "user-abort">;
  /** Human-readable detail for logs / the toast. */
  message: string;
  /** The phase the run is in at the failure. */
  phase: Phase;
  /** Best-effort results from stages that already finished, so the UI can still show measured work. */
  partial?: {
    download: ThroughputResult | null;
    upload: ThroughputResult | null;
    bidirectional: {
      down: ThroughputResult | null;
      up: ThroughputResult | null;
    } | null;
    latency: LatencyResult | null;
  };
  /** The original thrown value, for logging (not for display). */
  cause?: unknown;
}

/* Engine identity & capabilities ---------- Static self-description of a runner backend. */
export interface EngineInfo {
  /** Engine id, e.g. "real" | "dummy". */
  name: string;
  /* Per-engine version. */
  version: string;
  /* Transports this engine can drive for latency probing, preference order. */
  latencyTransports: TransportKind[];
  /* Transports this engine can drive for throughput transfer, preference order. */
  throughputTransports: TransportKind[];
}

/** Verified connection values are immutable inputs to one run, separate from live sockets. */
export interface VerifiedThroughputPath {
  requested: FetchThroughputTarget | WebTransportThroughputTarget;
  target: FetchThroughputTarget | WebTransportThroughputTarget;
  fetch: FetchThroughputTarget;
  probe: Probe;
  browserProtocol?: string;
  generation: string;
  verifiedAt: number;
}

export interface VerifiedLatencyPath {
  requested: LatencyTarget;
  target: LatencyTarget;
  probe: Probe;
  rttMs: number;
  generation: string;
  verifiedAt: number;
}

export interface PreparedPaths {
  discovery: TransportDiscovery;
  throughput: VerifiedThroughputPath;
  latency: VerifiedLatencyPath | null;
}

type TransportDiscoveryState =
  "advertised" | "browser-blocked" | "not-advertised";

/* One origin and every mechanism it advertises, in picker order. */
export interface DiscoveredTarget<T> {
  state: TransportDiscoveryState;
  targets: T[];
}

export type DiscoveredThroughput = DiscoveredTarget<
  FetchThroughputTarget | WebTransportThroughputTarget
>;

export type DiscoveredLatency = DiscoveredTarget<LatencyTarget>;

/* Server-advertised transports classified against the page that uses them. */
export interface TransportDiscovery {
  generation: string;
  engineVersion: string;
  server: { name: string; location?: string };
  fetchedAt: number;
  pageOrigin: string;
  pageSecure: boolean;
  pageProtocol?: string;
  throughput: Record<string, DiscoveredThroughput>;
  latency: Record<string, DiscoveredLatency>;
}

/* ---------- The event union the UI listens to ---------- */
export type RunnerEvent =
  | { type: "phase"; transition: PhaseTransition }
  | { type: "throughput"; sample: ThroughputSample }
  /* A short-lived upload-only visual target. */
  | { type: "uploadPresentation"; bytesPerSec: number | null }
  | { type: "latency"; sample: LatencyBucket }
  | {
      type: "latencySummary";
      stage: TransportRole;
      summary: StageLatencySummary | null;
    }
  // Reserved seam: a backend MAY push an explicit connectivity state.
  | { type: "connectivity"; state: ConnectivityState }
  // Progress within the active wall-time budget.
  | {
      type: "progress";
      phase: Phase;
      fraction: number; // 0..1 within the phase budget
      phaseElapsedMs: number;
      phaseBudgetMs: number;
      measuring: boolean; // false while delivery is stalled
    }
  | { type: "stability"; snapshot: StabilitySnapshot } // live stability; stalls report link health separately.
  | { type: "stall"; info: StallInfo }
  | { type: "resume" }
  // Transport negotiation telemetry: which connection method a phase is trying, and whether it is negotiating /.
  | { type: "stageSkipped"; failure: StageFailure }
  // Per-stage final result, emitted the instant each measured phase ends, so a finished stage shows its real result.
  | {
      type: "stageResult";
      stage: "download" | "upload";
      result: ThroughputResult;
    }
  | { type: "stageResult"; stage: "latency"; result: LatencyResult }
  | { type: "complete"; result: RunResult }
  // Abnormal end (user-abort is the "aborted" phase).
  | { type: "error"; error: RunnerError };

/* Connection and worker construction stay as built; these reshape only the remaining timeline or completion rule. */
export type LiveRunConfig = Pick<
  RunnerConfig,
  "stages" | "duration" | "adaptive"
>;

/* ---------- The contract ---------- */
export interface NetworkRunner {
  /** Connection preparation belongs to the application; RTT only adjusts warmup. */
  start(config: RunnerConfig, preTestPingMs: number): void;
  abort(): void;
  dispose(): void;
  on(handler: (e: RunnerEvent) => void): () => void;
  reconfigure(config: LiveRunConfig): void;
  readonly phase: Phase;
}

/* Stage lifecycle & warmup contract ---------- Connections belong to the STAGE, not the phase label. */
