// Shared runner contract: phases, config, events, result shapes, and backend
// interfaces used by both the UI and measurement engines.

import type {
  FetchThroughputTarget,
  LatencyTarget,
  WebTransportThroughputTarget,
} from "../api/endpoints";

/* ---------- Lifecycle ---------- */
/* Phase sequence, all stages on. Each enabled stage is preceded by its own
 * `warmup`, omitted when the stage is off or `duration.warmupMs <= 0`:
 *   idle → connecting → warmup → latency → warmup → download → warmup → upload → complete
 * See the stage lifecycle & warmup contract at the end of this file. */
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

/** Which way bytes flow for a throughput sample. Travels WITH the sample so the
 *  core never infers direction from the phase: one `bidirectional` phase then
 *  carries concurrent down+up samples unambiguously. */
export type FlowDirection = "down" | "up";
export type ProtocolTarget = "http1" | "http2" | "http3" | "negotiated";
export type ConnectionRole = "throughput" | "latency";
/** Advertised transfer target id; "current" resolves from the discovery hop. */
export type ThroughputTargetSelection = string;
export type PingCadence = "reply-driven" | "fast" | "medium" | "slow";

/* ---------- Phase activity descriptor (core → backend) ----------
 *  What a stage exercises, resolved ONCE by the scheduler from `RunnerConfig`
 *  and handed to the backend on every stage lifecycle hook (core.ts). A stage's
 *  warmup and its measured window carry the SAME activity object, so the warmup
 *  primes exactly the connections the measurement reuses. */
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

/* ---------- Overhead compensation ----------
 * Estimates forward-direction physical link occupancy from application bytes. */

/** Path the transfer takes: picks which overheads physically apply. Driven by
 *  the UI "Connection profile" preset. Loopback has no link layer, a tunnel adds
 *  outer encapsulation (see applyConnectionProfile in compensation.ts). */
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
/** Confidence-based early phase exit. `enabled: false` runs every phase for its
 *  full configured duration. */
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

/** Live stability snapshot for a measured phase: the single signal the pip,
 *  revocable early-finish confirmation, and result selection all read. */
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
  /** Enabled measured stages. `bidirectional` (concurrent down+up) defaults
   *  off; when on it runs after upload with its own warmup. */
  stages: {
    latency: boolean;
    download: boolean;
    upload: boolean;
    bidirectional: boolean;
  };
  /** When the latency stage is off, also skip the under-load latency pings
   *  taken during download/upload, so latency is fully off: no measurement, no
   *  profile, no chart line. */
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
  /** Lists the WebTransport datagram card in the picker. Filters that list and
   *  nothing else: the transport is a peer everywhere else, and a selected card
   *  stays listed and runnable with this off. */
  experimentalDatagramThroughput: boolean;
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
/** One authoritative in-run latency outcome in the window realm's monotonic
 * coordinate. The worker/channel boundary must translate its clock before the
 * outcome reaches RunnerCore. */
export interface LatencyObservation {
  rttMs: number;
  lost: boolean;
  observedAtMs: number;
}

export interface ThroughputSample {
  t: number; // ms since run start (monotonic)
  bytesPerSec: number; // smoothed live rate; exact results use private byte/time observations
  bytesCumulative: number;
  dir: FlowDirection; // which way these bytes flowed (down in download, up in upload, either in bidirectional)
  // The phase that produced this sample, stamped at ingest. Travels WITH the
  // sample (like `dir`) so consumers attribute it by tag, never re-deriving the
  // phase from timestamps. The single source of truth for sample→phase.
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
  underLoad: boolean; // true if captured during dl/ul (bufferbloat)
  // The phase that produced this ping (like ThroughputSample.phase). Pre-test
  // probe pings carry "idle"; in-run pings carry their measured phase. Lets the
  // LatencyProfile bucket lanes by tag, never by re-derived time windows.
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
  /** The bidirectional phase's two concurrent lanes, or null when the stage is
   *  off. Each lane reuses the same throughput reducer as download/upload. */
  bidirectional: {
    down: ThroughputResult | null;
    up: ThroughputResult | null;
  } | null;
  latency: LatencyResult | null;
  /** Unavailable unless both idle and loaded latency evidence exist. */
  bufferbloat: BufferbloatGrade | null;
  /** A usable result plus an entry here is a partial stage. */
  stageFailures: Partial<Record<TransportRole, StageFailure>>;
  startedAt: number; // epoch ms
  durationMs: number;
}

/** How a headline is derived. Throughput uses its final contiguous stable
 *  plateau when adaptive completion is enabled; otherwise it uses the full
 *  measured phase. Latency retains its adaptive arm-to-end median window. */
export type ResultMethod = "stable-window" | "full-average";

export interface ThroughputResult {
  meanBytesPerSec: number; // == reportedBytesPerSec, the headline value
  peakBytesPerSec: number;
  stabilityPct: number; // coefficient-of-variation based (0..100)
  totalBytes: number;
  reportedBytesPerSec: number; // effective bytes / represented time
  fullAverageBytesPerSec: number; // same effective whole-window rate
  method: ResultMethod;
  stabilityScore: number; // stability (0..1) at the moment the phase ends
  band: StabilityBand;
  /** Under-load ping timeout percentage; a quality signal, not TCP packet loss. */
  packetLossPct: number;
  /** True when bytes and time came from the server upload receiver. */
  serverAuthoritative?: boolean;
}

export interface LatencyResult {
  idleMs: number; // median unloaded over the chosen window, the headline
  minMs: number;
  p50Ms: number;
  p95Ms: number;
  jitterMs: number; // mean abs deviation
  packetLossPct: number;
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
/** Why a run ends abnormally. `user-abort` is the `"aborted"` phase instead, a
 *  deliberate stop; every other reason rides the `error` event below. Browser
 *  honesty: a server-initiated close and a network-level drop both surface as a
 *  generic fetch TypeError, so they collapse into `connection-lost`. */
export type TerminationReason =
  | "user-abort"
  | "preflight-failed" // the handshake never reaches, or a server rejects it
  | "connection-lost" // transport failed mid-run (server close or network loss)
  | "timeout" // a request/stream stalled past its deadline
  | "protocol-error" // malformed/unexpected response or close handshake
  | "internal-error" // a bug in the engine itself
  | "transport-unavailable"; // every negotiated transport failed to establish

/* ---------- Transport negotiation ---------- */
/** The connection method a backend may negotiate for a phase's I/O. A real
 *  engine tries these in preference order; each can fail to establish, and a
 *  failure of one is non-fatal as long as another succeeds. */
export type TransportKind =
  "webtransport" | "webtransport-datagram" | "websocket" | "fetch-stream";

/** The stage a transport is negotiated for. A backend negotiates once, at stage
 *  begin: the warmup primes it and the measured window reuses it, so there is no
 *  separate priming `warmup` role. Mirrors schedule's StagePhase, re-declared
 *  because contract.ts is the leaf types module and imports no schedule. */
export type TransportRole = Extract<
  Phase,
  "latency" | "download" | "upload" | "bidirectional"
>;

/** A stage that cannot run: the server lacks the capability, no transport can be
 *  negotiated, or the connection never establishes. NON-terminal, so the run
 *  continues with the remaining stages and the UI explains the gap in that
 *  stage's instrument (gauge for transfers, profile for latency). */
export interface StageFailure {
  stage: TransportRole;
  reason: Exclude<TerminationReason, "user-abort">;
  message: string;
}

/* ---------- Transient link health ---------- */
/** A NON-terminal stall: the link is quiet mid-phase and the run waits to
 *  reconnect. Elapsed time continues, so the gap affects throughput. Not a
 *  failure: a stall outliving MAX_STALL_MS becomes a `connection-lost`
 *  RunnerError instead. */
export interface StallInfo {
  reason: TerminationReason;
  transport?: TransportKind; // the connection that dropped, when known
  detail?: string;
}

/** A structured run failure, carried on the `error` event. Distinguishing a
 *  failure from a user abort (the `"aborted"` phase) and from a clean finish is
 *  the runner→webapp half of the lifecycle contract. */
export interface RunnerError {
  /** Failure category; `user-abort` is the `"aborted"` phase instead. */
  reason: Exclude<TerminationReason, "user-abort">;
  /** Human-readable detail for logs / the toast. */
  message: string;
  /** The phase the run is in at the failure. */
  phase: Phase;
  /** Best-effort results from stages that already finished, so the UI can still
   *  show measured work. */
  partial?: {
    download: ThroughputResult | null;
    upload: ThroughputResult | null;
    latency: LatencyResult | null;
  };
  /** The original thrown value, for logging (not for display). */
  cause?: unknown;
}

/* ---------- Engine identity & capabilities ----------
 *  Static self-description of a runner backend. Capabilities live on the
 *  ENGINE: one engine drives many transports, and the user picks per role from
 *  these lists. The Endpoint info panel renders them. */
export interface EngineInfo {
  /** Engine id, e.g. "real" | "dummy". */
  name: string;
  /** Per-engine version. Both built-ins report the client build version because
   *  both ship with it; a separately shipped engine reports its own. */
  version: string;
  /** Transports this engine can drive for latency probing, preference order.
   *  A message bus: websocket, or webtransport datagrams. */
  latencyTransports: TransportKind[];
  /** Transports this engine can drive for throughput transfer, preference
   *  order. Byte lanes: fetch streams over h1.1/h2/h3, or webtransport streams.
   *  Websocket is never a throughput transport. */
  throughputTransports: TransportKind[];
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
  server: { name: string; location?: string };
  preTestPingMs: number;
  engineVersion: string;
  discoveryGeneration: string;
  protocolNegotiated: string;
  selectedThroughputTarget?: string;
  selectedThroughputProtocol?: ProtocolTarget;
  selectedThroughputTransport?: TransportKind;
  selectedLatencyTarget?: string;
  selectedLatencyTransport?: TransportKind;
  latencyProtocolNegotiated?: string;
  /** Browser-facing protocol from Resource Timing (e.g. http/1.1, h2, h3). */
  firstHopProtocol?: string;
  firstHopSecure?: boolean;
  /** Measurement occupancy the server reported at probe time. Concurrent tests
   *  contend for bandwidth and CPU, so a busy server means results may be
   *  affected. */
  serverLoad?: { active: number; max: number };
}

export type TransportDiscoveryState =
  "advertised" | "browser-blocked" | "not-advertised";

/** One origin and every mechanism it advertises, in picker order. A proxy
 *  serving TCP and UDP on one hostname appears once per mechanism, so a client
 *  that cannot reach UDP still resolves the others. */
export interface DiscoveredTarget<T> {
  state: TransportDiscoveryState;
  targets: T[];
}

export type DiscoveredThroughput = DiscoveredTarget<
  FetchThroughputTarget | WebTransportThroughputTarget
>;

export type DiscoveredLatency = DiscoveredTarget<LatencyTarget>;

/** Server-advertised transports classified against the page that uses them.
 * Emitted as soon as /preflight completes, ahead of selection and probing. */
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
  | { type: "transportDiscovery"; discovery: TransportDiscovery }
  | { type: "infra"; info: InfraInfo }
  | { type: "phase"; transition: PhaseTransition }
  | { type: "throughput"; sample: ThroughputSample }
  | { type: "latency"; sample: LatencyBucket }
  // Reserved seam: a backend MAY push an explicit connectivity state. The store
  // otherwise derives `effectiveConnectivity` from loss/jitter/measuring, so
  // this is an optional override for an engine with a better signal.
  | { type: "connectivity"; state: ConnectivityState }
  // Progress within the active wall-time budget. `measuring` is false while
  // delivery is stalled; the grind-to-zero presentation keys off this flag.
  | {
      type: "progress";
      phase: Phase;
      fraction: number; // 0..1 within the phase budget
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
  // Transport negotiation telemetry: which connection method a phase is trying,
  // and whether it is negotiating / established / failed. The core re-emits it
  // verbatim; the store records it.
  // A stage is skipped because it cannot run (see StageFailure). The rest of
  // the run continues; the UI surfaces the reason in the stage's instrument.
  | { type: "stageSkipped"; failure: StageFailure }
  // Per-stage final result, emitted the instant each measured phase ends, so a
  // finished stage shows its real result while later stages still run. Stages
  // are independent: each carries its own headline/method/band.
  | {
      type: "stageResult";
      stage: "download" | "upload";
      result: ThroughputResult;
    }
  | { type: "stageResult"; stage: "latency"; result: LatencyResult }
  | { type: "complete"; result: RunResult }
  // Abnormal end (user-abort is the "aborted" phase). Structured so the UI can
  // tell preflight-unreachable from a mid-run drop and surface any partial
  // results. See RunnerError.
  | { type: "error"; error: RunnerError };

/* ---------- Runtime anomaly injection: Developer panel ---------- */
/** A live, dev-only perturbation fired into a *running* engine. Unlike the
 *  construction-time `DummyOptions.anomalies` (phase fractions), these fire
 *  relative to the current moment in the active phase. The Settings Developer
 *  panel triggers them via `injectAnomaly` in engine.svelte.ts. */
export type RunnerAnomaly =
  | { kind: "latency-spike"; magnitude?: number; durationMs?: number } // rtt ×magnitude
  | { kind: "packet-loss"; magnitude?: number; durationMs?: number } // loss probability
  | { kind: "throughput-drop"; magnitude?: number; durationMs?: number } // bytesPerSec ×(1−magnitude)
  // A full connection drop (dead air): the backend host.stall()s immediately
  // and host.resume()s durationMs later. Makes the stall/grind-to-zero scenario
  // visually testable with the dummy.
  | { kind: "connection-drop"; durationMs?: number };

/** Settings the core can safely apply mid-run. Connection and worker
 * construction stay as built; these only reshape the remaining timeline or its
 * completion rule. */
export type LiveRunConfig = Pick<
  RunnerConfig,
  "stages" | "duration" | "adaptive"
>;

/* ---------- The contract ---------- */
export interface NetworkRunner {
  /** Verify the selected target, then run. Emits `connecting` immediately so
   *  asynchronous path verification is visible and cancellable. `prepared` is
   *  the InfraInfo an earlier probe() already resolved; omitting it makes start
   *  probe itself. The app always has one (validateConnections runs first), so
   *  the internal probe serves a caller holding this interface alone. */
  start(config: RunnerConfig, prepared?: InfraInfo): Promise<void>;
  abort(): void;
  /** Permanently stop background activity owned by this runner. */
  dispose?(): void;
  /** Suspend or resume the idle keepalive. A hidden tab suspends it so the
   *  browser can park the page; a run is never affected. */
  setBackgroundActivity?(enabled: boolean): void;
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
  /** OPTIONAL: fire a live anomaly into an in-flight run. Optional so a minimal
   *  real engine need not implement it. */
  injectAnomaly?(a: RunnerAnomaly): void;
  readonly phase: Phase;
}

/* ---------- Stage lifecycle & warmup contract ----------
 *  Connections belong to the STAGE, not the phase label: `RunnerBackend` in
 *  core.ts drives begin/measure/end once per stage. One `duration.warmupMs`
 *  window precedes every stage, reaching the UI as the generic `"warmup"` phase,
 *  and it starts only once asynchronous preparation resolves. */
