/* ============================================================
 * The Graphite Meter — Reactive Store (§2.3 + §2.4)
 * Single source of truth. The UI binds to derived display
 * values, never to raw event streams directly. Ring buffers
 * cap memory and feed the canvas (read via rAF — NOT $effect).
 * ============================================================ */

import type {
  RunnerEvent,
  Phase,
  ConnectivityState,
  InfraInfo,
  RunResult,
  RunnerConfig,
  ThroughputSample,
  LatencySample,
} from "../runner/contract";

/* ================= DEFAULTS (§2.4) ================= */

export const DEFAULT_CONFIG: RunnerConfig = {
  stages: { latency: true, download: true, upload: true },
  duration: { warmupMs: 1500, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000 },
  transport: { transfer: "webtransport", latency: "websocket" },
  pingConcurrency: "medium",
  parallelStreams: 4,
  endpoint: { host: "auto", port: 443, path: "/measure" },
  // ----- Ported config surface (§13.1); inert until Batches C/D consume it -----
  compensation: {
    enabled: true,
    factors: {
      ethernetFraming: true,
      tlsRecords: true,
      applicationFraming: true,
      reversePathControl: true,
      lossRetransmission: true,
      steadyStateRamp: true,
      browserRuntime: true,
    },
    params: {
      mtuBytes: 1500,
      vlanTagged: false,
      tcpOptionsBytes: 12,
      framePayloadBytes: 16384,
      tlsRecordBytes: 5,
      aeadTagBytes: 16,
      quicConnIdBytes: 8,
      maxLossRatio: 0.12,
    },
  },
  adaptive: {
    enabled: false, // off by default → durations behave as the base build
    minCoverageRatio: 0.52,
    stabilityThreshold: 0.86,
    maxPhaseReductionRatio: 0.5,
    minLatencySamples: 8,
    minTransferSamples: 12,
  },
  visualization: { throughputMaxBps: "auto" },
};

export const DURATION_PRESETS = {
  short: { warmupMs: 1000, latencyMs: 2500, downloadMs: 5000, uploadMs: 5000 },
  medium: { warmupMs: 1500, latencyMs: 4000, downloadMs: 10000, uploadMs: 10000 },
  long: { warmupMs: 2000, latencyMs: 6000, downloadMs: 20000, uploadMs: 20000 },
} as const;

const MAX_SAMPLES = 1200; // ~ enough for a 60s run at 16Hz, ring-buffered

class ConsoleStore {
  /* ---- raw ingest (ring buffers) ---- */
  throughput = $state<ThroughputSample[]>([]);
  latency = $state<LatencySample[]>([]);

  /* ---- lifecycle ---- */
  phase = $state<Phase>("idle");
  phaseFraction = $state(0); // 0–1 within current phase
  connectivity = $state<ConnectivityState>("connected");
  infra = $state<InfraInfo | null>(null);
  result = $state<RunResult | null>(null);
  errorMsg = $state<string | null>(null);
  startEpoch = $state(0);

  /* ---- config (bound to settings UI) ---- */
  config = $state<RunnerConfig>(structuredClone(DEFAULT_CONFIG));

  /* ---- display preferences ---- */
  unitBase = $state<"base10" | "base2">("base10"); // Mbps vs Mibit/s
  unitKind = $state<"bits" | "bytes">("bits");

  /* ================= DERIVED ================= */

  /** The single big number shown in the Reactor, in the active unit. */
  liveMetric = $derived.by(() => {
    const last = this.#lastSampleForPhase();
    if (!last) return { value: 0, unit: this.unitLabel };
    return { value: this.toUnit(last.bps), unit: this.unitLabel };
  });

  /** Most recent rtt for the connectivity pulse + live ping. */
  liveRtt = $derived(
    this.latency.length
      ? this.latency.at(-1)!.rttMs
      : (this.infra?.preTestPingMs ?? 0),
  );

  /** Rolling packet loss over last 20 latency samples (for pulse state). */
  rollingLossPct = $derived.by(() => {
    const w = this.latency.slice(-20);
    if (!w.length) return 0;
    return (w.filter((s) => s.lost).length / w.length) * 100;
  });

  jitterMs = $derived.by(() => {
    const w = this.latency.slice(-30).filter((s) => !s.lost);
    if (w.length < 2) return 0;
    let acc = 0;
    for (let i = 1; i < w.length; i++) acc += Math.abs(w[i].rttMs - w[i - 1].rttMs);
    return acc / (w.length - 1);
  });

  /** UI computes connectivity if runner doesn't push it (defensive). */
  effectiveConnectivity = $derived.by<ConnectivityState>(() => {
    if (this.phase === "error") return "offline";
    if (this.connectivity === "offline") return "offline";
    if (this.rollingLossPct > 5) return "unstable";
    if (this.rollingLossPct > 0.5 || this.jitterMs > 30) return "degraded";
    return "connected";
  });

  elapsedMs = $derived(this.startEpoch ? Date.now() - this.startEpoch : 0);

  bytesTransferred = $derived(this.throughput.at(-1)?.bytesCumulative ?? 0);

  isRunning = $derived(
    !["idle", "complete", "aborted", "error"].includes(this.phase),
  );

  get unitLabel() {
    const speed =
      this.unitKind === "bits"
        ? this.unitBase === "base10"
          ? "Mbps"
          : "Mibit/s"
        : this.unitBase === "base10"
          ? "MB/s"
          : "MiB/s";
    return speed;
  }

  toUnit(bps: number): number {
    const div = this.unitBase === "base10" ? 1e6 : 2 ** 20;
    const v = this.unitKind === "bits" ? bps / div : bps / 8 / div;
    return v;
  }

  /* ================= INGEST ================= */
  ingest = (e: RunnerEvent) => {
    switch (e.type) {
      case "infra":
        this.infra = e.info;
        break;
      case "phase":
        this.phase = e.transition.to;
        this.phaseFraction = 0;
        if (e.transition.to === "warmup") this.startEpoch = Date.now();
        break;
      case "progress":
        this.phaseFraction = e.fraction;
        break;
      case "throughput":
        this.throughput.push(e.sample);
        if (this.throughput.length > MAX_SAMPLES) this.throughput.shift();
        break;
      case "latency":
        this.latency.push(e.sample);
        if (this.latency.length > MAX_SAMPLES) this.latency.shift();
        break;
      case "connectivity":
        this.connectivity = e.state;
        break;
      case "complete":
        this.result = e.result;
        this.phase = "complete";
        break;
      case "error":
        this.errorMsg = e.message;
        this.phase = "error";
        break;
    }
  };

  reset() {
    this.throughput = [];
    this.latency = [];
    this.phase = "idle";
    this.phaseFraction = 0;
    this.result = null;
    this.errorMsg = null;
    this.startEpoch = 0;
  }

  #lastSampleForPhase() {
    return this.throughput.at(-1) ?? null;
  }
}

export const console = new ConsoleStore();
