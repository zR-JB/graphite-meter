/* ============================================================
 * The Graphite Meter — Runner Contract (§2.1)
 * Types only. The UI is engine-agnostic: it consumes events
 * from any object implementing `NetworkRunner`.
 * ============================================================ */

/* ---------- Lifecycle ---------- */
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

/* ---------- Configuration passed INTO the runner ---------- */
export interface RunnerConfig {
  stages: { latency: boolean; download: boolean; upload: boolean };
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
}

/* ---------- Raw samples emitted DURING a run ---------- */
export interface ThroughputSample {
  t: number; // ms since run start (monotonic)
  bps: number; // instantaneous bits/sec (raw, base-10 neutral)
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
  meanBps: number;
  peakBps: number;
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

/* ---------- The contract ---------- */
export interface NetworkRunner {
  start(config: RunnerConfig): void;
  abort(): void;
  /** Pre-test handshake; resolves InfraInfo. Pings every `intervalMs`. */
  probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo>;
  on(handler: (e: RunnerEvent) => void): () => void; // returns unsubscribe
  readonly phase: Phase;
}
