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
  | "complete"
  | "aborted"
  | "error";

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

/* ---------- Configuration passed INTO the runner ---------- */
export interface RunnerConfig {
  stages: { latency: boolean; download: boolean; upload: boolean };
  /** When the latency stage is off, also skip the under-load latency pings
   *  taken during download/upload — so latency is fully off (no measurement,
   *  no profile, no chart line) rather than just dropping the idle phase. */
  skipLoadedLatencyWhenStageOff: boolean;
  duration: {
    warmupMs: number;
    latencyMs: number;
    downloadMs: number;
    uploadMs: number;
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
}

export interface LatencySample {
  t: number;
  rttMs: number;
  underLoad: boolean; // true if captured during dl/ul (bufferbloat)
  lost: boolean; // packet considered lost
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
  latency: LatencyResult;
  bufferbloat: BufferbloatGrade;
  startedAt: number; // epoch ms
  durationMs: number;
}

export interface ThroughputResult {
  meanBytesPerSec: number;
  peakBytesPerSec: number;
  stabilityPct: number; // coefficient-of-variation based (0–100)
  totalBytes: number;
}

export interface LatencyResult {
  idleMs: number; // median unloaded
  minMs: number;
  p50Ms: number;
  p95Ms: number;
  jitterMs: number; // mean abs deviation
  packetLossPct: number;
}

export interface BufferbloatGrade {
  grade: "A" | "B" | "C" | "D" | "F";
  idleMs: number;
  loadedMs: number;
  increaseMs: number; // loaded − idle
}

/* ---------- Pre-test handshake info ---------- */
export interface InfraInfo {
  clientIp: string;
  server: { name: string; host: string; port: number; location?: string };
  preTestPingMs: number;
  engineVersion: string;
  protocolNegotiated: string;
}

/* ---------- The event union the UI listens to ---------- */
export type RunnerEvent =
  | { type: "infra"; info: InfraInfo }
  | { type: "phase"; transition: PhaseTransition }
  | { type: "throughput"; sample: ThroughputSample }
  | { type: "latency"; sample: LatencySample }
  | { type: "connectivity"; state: ConnectivityState }
  | { type: "progress"; phase: Phase; fraction: number } // 0–1 within phase
  | { type: "complete"; result: RunResult }
  | { type: "error"; message: string };

/* ---------- Runtime anomaly injection (§13.6 — Developer panel) ---------- */
/** A live, dev-only perturbation fired into a *running* engine. Unlike the
 *  construction-time `DummyOptions.anomalies` (phase fractions), these fire
 *  relative to the current moment in the active phase — the Workbench
 *  Developer panel triggers them via `wire.injectAnomaly`. */
export type RunnerAnomaly =
  | { kind: "latency-spike"; magnitude?: number; durationMs?: number } // rtt ×magnitude
  | { kind: "packet-loss"; magnitude?: number; durationMs?: number } // loss probability
  | { kind: "throughput-drop"; magnitude?: number; durationMs?: number }; // bytesPerSec ×(1−magnitude)

/* ---------- The contract ---------- */
export interface NetworkRunner {
  start(config: RunnerConfig): void;
  abort(): void;
  /** Pre-test handshake; resolves InfraInfo. Pings every `intervalMs`. */
  probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo>;
  on(handler: (e: RunnerEvent) => void): () => void; // returns unsubscribe
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
