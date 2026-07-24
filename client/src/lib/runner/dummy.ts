// Deterministic development sample source.

import type {
  RunnerConfig,
  PingCadence,
  RunnerAnomaly,
  InfraInfo,
  EngineInfo,
  FlowDirection,
  PhaseActivity,
} from "./contract";
import type { CoreHost, RunnerBackend } from "./core";
import { needsPings, ROUTES } from "./real/backendPure";
import { BUILD } from "../buildenv";

export interface DummyOptions {
  seed?: number;
  /** Inject anomalies at fractions of each phase: */
  anomalies?: {
    packetDropAt?: number[]; // e.g. [0.4, 0.7] → drop bursts
    latencySpikeAt?: number[]; // e.g. [0.5] → 3× rtt spike
    throughputDipAt?: number[]; // e.g. [0.6] → 400ms 40% drop
  };
  /** Realistic target profiles: */
  profile?: "fiber" | "cable" | "lte" | "satellite" | "throttled";
}

/* ---------- Profile table: mean throughput + latency character ---------- */
interface ProfileSpec {
  downBytesPerSec: number;
  upBytesPerSec: number;
  idleRttMs: number;
  /** RTT increase under load: drives the bufferbloat grade. */
  loadedDeltaMs: number;
  /** Baseline loss probability per ping. */
  lossBase: number;
  /** Relative std of the plateau (throughput and idle RTT): how steady the link
   *  is. The adaptive stability score reads it, so steady links (fiber/cable)
   *  finish early while jittery ones (lte/satellite) use the full window. */
  jitter: number;
}

// Throughput is bytes/sec (browser-native). Link rates are conventionally
// quoted in bits/sec, so the trailing comment notes the familiar bit-rate.
const PROFILES: Record<NonNullable<DummyOptions["profile"]>, ProfileSpec> = {
  fiber: {
    downBytesPerSec: 117.5e6,
    upBytesPerSec: 110e6,
    idleRttMs: 6,
    loadedDeltaMs: 4,
    lossBase: 0.0,
    jitter: 0.04,
  }, // ~940/880 Mbit/s
  cable: {
    downBytesPerSec: 40e6,
    upBytesPerSec: 2.75e6,
    idleRttMs: 16,
    loadedDeltaMs: 34,
    lossBase: 0.002,
    jitter: 0.05,
  }, // ~320/22 Mbit/s
  lte: {
    downBytesPerSec: 8e6,
    upBytesPerSec: 3e6,
    idleRttMs: 38,
    loadedDeltaMs: 62,
    lossBase: 0.01,
    jitter: 0.09,
  }, // ~64/24 Mbit/s
  satellite: {
    downBytesPerSec: 13.75e6,
    upBytesPerSec: 1.75e6,
    idleRttMs: 600,
    loadedDeltaMs: 180,
    lossBase: 0.015,
    jitter: 0.11,
  }, // ~110/14 Mbit/s
  throttled: {
    downBytesPerSec: 1.1875e6,
    upBytesPerSec: 0.5625e6,
    idleRttMs: 28,
    loadedDeltaMs: 48,
    lossBase: 0.005,
    jitter: 0.05,
  }, // ~9.5/4.5 Mbit/s
};

const PING_INTERVAL: Record<PingCadence, number> = {
  // The dummy has no request/reply transport; one core tick approximates the
  // reply-driven worker's UI-visible sample stream.
  "reply-driven": 20,
  fast: 80,
  medium: 250,
  slow: 600,
};

const THROUGHPUT_CADENCE_MS = 100;

export interface DummySampleContext {
  activity: PhaseActivity;
  measuring: boolean;
  elapsed: number;
  segStart: number;
  segEnd: number;
  realNow: number;
}

/* ---------- Live anomaly defaults ----------
 * Defaults for RUNTIME anomalies injected mid-run via `injectAnomaly`. Each
 * occupies an absolute [start,end) window in ms since run start, anchored at
 * the instant the Developer button fires. Construction-time
 * `DummyOptions.anomalies` fire at phase fractions instead. */
const LIVE_ANOMALY_DEFAULTS = {
  latencySpike: { magnitude: 3, durationMs: 600 }, // rtt ×3 for 600ms
  packetLoss: { magnitude: 0.6, durationMs: 900 }, // 60% loss probability
  throughputDrop: { magnitude: 0.4, durationMs: 600 }, // bytesPerSec −40%
  connectionDrop: { durationMs: 4000 }, // full dead-air drop, then reconnect
} as const;

/** A scheduled live anomaly with an absolute window on the run timeline. */
interface LiveAnomaly {
  kind: RunnerAnomaly["kind"];
  start: number; // ms since run start
  end: number;
  magnitude: number;
}

/* ---------- Deterministic RNG (mulberry32 + Box-Muller) ---------- */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export class DummyBackend implements RunnerBackend {
  #opts: Required<Pick<DummyOptions, "profile">> & DummyOptions;
  #spec: ProfileSpec;
  #rand: () => number;
  #host: CoreHost | null = null;

  // Wall-time gates prevent adaptive glides from emitting sample bursts.
  #lastThroughputAt = -Infinity;
  #lastPingAt = -Infinity;
  #sampleTimer: ReturnType<typeof setTimeout> | null = null;
  #activity: PhaseActivity | null = null;
  #segmentStart = 0;

  // Live, dev-injected anomalies. Each is an absolute [start,end) window
  // on the effective timeline; the synthesis hooks read this list.
  #liveAnomalies: LiveAnomaly[] = [];

  // A live connection-drop window in wall time. It contributes zero-byte
  // duration samples without proving delivery, then resumes at the window end.
  #dropEndReal = 0; // monotonic time the drop lifts at, or 0 when not dropped
  #dropLastReal = 0;

  constructor(opts: DummyOptions = {}) {
    this.#opts = { profile: opts.profile ?? "fiber", ...opts };
    this.#spec = PROFILES[this.#opts.profile];
    this.#rand = mulberry32(opts.seed ?? 0x9e3779b9);
  }

  attach(host: CoreHost): void {
    this.#host = host;
  }

  /* ---------- Gaussian noise via Box-Muller ---------- */
  #gauss(): number {
    const u = Math.max(1e-9, this.#rand());
    const v = this.#rand();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  }

  /* ================= PROBE ================= */
  async probe(_config: RunnerConfig, signal?: AbortSignal): Promise<InfraInfo> {
    signal?.throwIfAborted();
    const pageOrigin =
      typeof location === "undefined" ? "http://localhost" : location.origin;
    const secure = pageOrigin.startsWith("https:");
    const throughputId = secure ? "http1-tls" : "http1-clear";
    const latencyId = secure ? "ws-http1-tls" : "ws-http1-clear";
    this.#host?.emit({
      type: "transportDiscovery",
      discovery: {
        generation: "dummy",
        engineVersion: "dummy-1.0.0",
        server: {
          name: "Graphite Edge — Frankfurt",
          location: "Frankfurt, DE",
        },
        fetchedAt: Date.now(),
        pageOrigin,
        pageSecure: secure,
        pageProtocol: "http/1.1",
        throughput: {
          [throughputId]: {
            state: "advertised",
            target: {
              id: throughputId,
              origin: pageOrigin,
              transport: "fetch-stream",
              protocol: "http1",
              tls: secure,
              routes: {
                probe: ROUTES.probe,
                download: ROUTES.download,
                upload: ROUTES.upload,
                uploadSession: ROUTES.uploadSession,
                uploadProgress: ROUTES.uploadProgress,
              },
            },
          },
        },
        latency: {
          [latencyId]: {
            state: "advertised",
            target: {
              id: latencyId,
              origin: pageOrigin,
              transport: "websocket",
              protocol: "http1",
              tls: secure,
              routes: { probe: ROUTES.probe, ping: ROUTES.ping },
            },
          },
        },
      },
    });
    const interval = 90;
    const pings = 4;
    // Emit a few pre-test pings so the sparkline has something to show. These
    // are pre-run telemetry, emitted directly (not accumulated into a result).
    for (let i = 0; i < pings; i++) {
      await new Promise((r) => setTimeout(r, interval));
      const rtt = this.#spec.idleRttMs * (1 + this.#gauss() * 0.08);
      this.#host?.emit({
        type: "latency",
        // Pre-test idle pings: phase "idle" (negative t), so the LatencyProfile's
        // idle lane (phase==="latency") excludes them while the sparkline shows them.
        sample: {
          t: -interval * (pings - i),
          rttMs: rtt,
          underLoad: false,
          lost: false,
          phase: "idle",
        },
      });
    }

    const octet = () => Math.floor(this.#rand() * 254) + 1;
    return {
      clientIp: `${octet()}.${octet()}.${octet()}.${octet()}`,
      clientIpVersion: 4,
      clientIpSource: "socket",
      server: {
        name: "Graphite Edge — Frankfurt",
        location: "Frankfurt, DE",
      },
      preTestPingMs: this.#spec.idleRttMs,
      engineVersion: "dummy-1.0.0",
      discoveryGeneration: "dummy",
      protocolNegotiated: "http/1.1",
      selectedThroughputTarget: throughputId,
      selectedThroughputProtocol: "http1",
      selectedLatencyTarget: latencyId,
      selectedThroughputTransport: "fetch-stream",
      selectedLatencyTransport: "websocket",
      latencyProtocolNegotiated: "http/1.1",
      firstHopProtocol: "http/1.1",
      firstHopSecure: secure,
    };
  }

  /** Synthetic capabilities: the dummy "supports" every per-role transport,
   *  exercising the full capability surface the UI renders. Everything here is
   *  simulated. WebSocket appears only under latency: it is the ping bus, never
   *  a byte-transfer lane. */
  describe(): EngineInfo {
    return {
      name: "dummy",
      version: BUILD.clientVersion,
      latencyTransports: ["webtransport-datagrams", "websocket"],
      throughputTransports: ["webtransport-streams", "fetch-streams"],
    };
  }

  /* ================= LIFECYCLE (core → backend) ================= */
  onRunStart(_config: RunnerConfig): void {
    this.#stopSamples();
    this.#lastThroughputAt = -Infinity;
    this.#lastPingAt = -Infinity;
    this.#liveAnomalies = [];
    this.#dropEndReal = 0;
  }

  onStageBegin(_activity: PhaseActivity): void {
    this.#stopSamples();
  }

  onStageMeasure(activity: PhaseActivity): void {
    const host = this.#host;
    const cfg = host?.config;
    if (!host || !cfg) return;
    this.#stopSamples();
    this.#activity = activity;
    this.#segmentStart = host.elapsed;
    this.#scheduleSample(0);
  }

  onStageEnd(_activity: PhaseActivity): void {
    this.#stopSamples();
  }

  onComplete(): void {
    this.#stopSamples();
  }

  onAbort(): void {
    this.#stopSamples();
  }

  /** Fallback idle RTT for an empty-sample run: the profile's idle ping. */
  idleHintMs(): number {
    return this.#spec.idleRttMs;
  }

  sample(ctx: DummySampleContext): void {
    const cfg = this.#host?.config;
    if (!cfg) return;
    const { activity, measuring, elapsed, segStart, segEnd, realNow } = ctx;

    // Account dead air as zero-byte wall time without falsely resuming delivery.
    if (this.#dropEndReal > 0) {
      const seconds = Math.max(0, realNow - this.#dropLastReal) / 1000;
      this.#dropLastReal = realNow;
      for (const dir of activity.transfer)
        this.#host!.ingestThroughput(dir, 0, 0, seconds);
      if (realNow < this.#dropEndReal) return;
      this.#dropEndReal = 0;
      this.#dropLastReal = 0;
      this.#host!.resume();
      // Snap the sample cadence gates to realNow so resume dumps no backlog.
      this.#lastThroughputAt = realNow;
      this.#lastPingAt = realNow;
    }

    // The warmup window primes real connections; the dummy has none, so it emits
    // nothing until measurement begins, like a real backend at onStageMeasure.
    if (!measuring) return;

    // Throughput on the stage's transfer lanes. The cadence gates on REAL time:
    // measured time races ahead during an early-finish glide.
    if (
      activity.transfer.length > 0 &&
      realNow - this.#lastThroughputAt >= THROUGHPUT_CADENCE_MS
    ) {
      this.#lastThroughputAt = realNow;
      for (const dir of activity.transfer) {
        this.#synthThroughput(dir, elapsed, segStart, segEnd);
      }
    }

    // Idle-stage pings, or loaded (bufferbloat) pings during a transfer stage.
    // `activity.loadedLatency` folds in the skip rule, resolved by the scheduler.
    const pingInterval =
      PING_INTERVAL[
        activity.stage === "latency" ? cfg.pingCadence : cfg.loadedPingCadence
      ];
    if (needsPings(activity) && realNow - this.#lastPingAt >= pingInterval) {
      this.#lastPingAt = realNow;
      this.#synthLatency(activity, elapsed, segStart, segEnd);
    }
  }

  #scheduleSample(delay: number): void {
    this.#sampleTimer = setTimeout(() => {
      this.#sampleTimer = null;
      const host = this.#host;
      const activity = this.#activity;
      const cfg = host?.config;
      if (!host || !activity || !cfg || host.phase !== activity.stage) return;
      const now = performance.now();
      this.sample({
        activity,
        measuring: true,
        elapsed: host.elapsed,
        segStart: this.#segmentStart,
        segEnd: this.#segmentStart + cfg.duration[`${activity.stage}Ms`],
        realNow: now,
      });
      const pingActive = needsPings(activity);
      const pingInterval = pingActive
        ? PING_INTERVAL[
            activity.stage === "latency"
              ? cfg.pingCadence
              : cfg.loadedPingCadence
          ]
        : Infinity;
      const next = Math.min(
        activity.transfer.length
          ? this.#lastThroughputAt + THROUGHPUT_CADENCE_MS
          : Infinity,
        pingActive ? this.#lastPingAt + pingInterval : Infinity,
        this.#dropEndReal > 0
          ? Math.min(this.#dropEndReal, now + 100)
          : Infinity,
      );
      if (Number.isFinite(next)) this.#scheduleSample(Math.max(1, next - now));
    }, delay);
  }

  #stopSamples(): void {
    if (this.#sampleTimer) clearTimeout(this.#sampleTimer);
    this.#sampleTimer = null;
    this.#activity = null;
  }

  /* ---------- Throughput sample synthesis ---------- */
  // `dir` (not the phase) picks the rate, so the bidirectional phase synthesizes
  // a down lane and an up lane from the same profile per tick.
  #synthThroughput(
    dir: FlowDirection,
    elapsed: number,
    segStart: number,
    segEnd: number,
  ) {
    const tp = elapsed - segStart; // ms into this phase
    const phaseLen = segEnd - segStart;
    const mean =
      dir === "down" ? this.#spec.downBytesPerSec : this.#spec.upBytesPerSec;

    // Logistic ramp-up over the first ~1.2s.
    const ramp = 1 / (1 + Math.exp(-(tp - 600) / 150));

    // Noisy plateau, Gaussian noise scaled by the profile's steadiness.
    let bytesPerSec = mean * ramp * (1 + this.#gauss() * this.#spec.jitter);

    // Throughput dip anomaly: 400ms 40% drop centred on each fraction.
    for (const f of this.#opts.anomalies?.throughputDipAt ?? []) {
      const center = f * phaseLen;
      if (tp >= center && tp < center + 400) bytesPerSec *= 0.6;
    }

    // Live throughput-drop anomaly: cut bytesPerSec by `magnitude` in-window.
    const drop = this.#activeAnomaly("throughput-drop", elapsed);
    if (drop) bytesPerSec *= Math.max(0, 1 - drop.magnitude);

    bytesPerSec = Math.max(0, bytesPerSec);

    // Bytes over the cadence window (rate is bytes/sec). The core accumulates
    // them and tracks the total; direction travels with the sample.
    const bytes = bytesPerSec * (THROUGHPUT_CADENCE_MS / 1000);
    this.#host!.ingestThroughput(
      dir,
      bytesPerSec,
      bytes,
      THROUGHPUT_CADENCE_MS / 1000,
    );
  }

  /* ---------- Latency sample synthesis ---------- */
  // `activity.transfer` (not the phase) decides under-load: a stage that moves
  // bytes produces loaded (bufferbloat) pings; the latency stage produces idle.
  #synthLatency(
    activity: PhaseActivity,
    elapsed: number,
    segStart: number,
    segEnd: number,
  ) {
    const tp = elapsed - segStart;
    const phaseLen = segEnd - segStart;
    const frac = tp / phaseLen;
    const underLoad = activity.transfer.length > 0;

    let rtt = this.#spec.idleRttMs;
    if (underLoad) {
      // Under-load latency rises toward the profile's loaded delta.
      const loadRamp = 1 / (1 + Math.exp(-(tp - 500) / 200));
      rtt += this.#spec.loadedDeltaMs * loadRamp;
    }
    rtt *= 1 + this.#gauss() * this.#spec.jitter; // jitter scaled by steadiness

    // Latency spike anomaly: 3× RTT near each fraction.
    for (const f of this.#opts.anomalies?.latencySpikeAt ?? []) {
      if (Math.abs(frac - f) < 0.02) rtt *= 3;
    }

    // Live latency-spike anomaly: scale rtt by `magnitude` in-window.
    const spike = this.#activeAnomaly("latency-spike", elapsed);
    if (spike) rtt *= spike.magnitude;

    // Packet loss: baseline + burst windows.
    let lossProb = this.#spec.lossBase;
    for (const f of this.#opts.anomalies?.packetDropAt ?? []) {
      if (Math.abs(frac - f) < 0.03) lossProb = 0.6;
    }
    // Live packet-loss anomaly: raise loss probability in-window.
    const loss = this.#activeAnomaly("packet-loss", elapsed);
    if (loss) lossProb = Math.max(lossProb, loss.magnitude);
    const lost = this.#rand() < lossProb;

    this.#host!.ingestLatency(rtt, underLoad, lost);
  }

  /* ================= LIVE ANOMALY INJECTION ================= */
  /**
   * Fire a dev-only anomaly into the in-flight run. It opens an absolute window
   * starting at the current elapsed and is consumed by the synthesis hooks.
   * No-op when not running (no host config), so the Developer panel's disabled
   * state mirrors `isRunning`.
   */
  injectAnomaly(a: RunnerAnomaly): void {
    const host = this.#host;
    if (!host || !host.config || host.phase === "idle") return;

    // Connection drops use wall time so adaptive timeline glides cannot shorten them.
    if (a.kind === "connection-drop") {
      const durationMs =
        a.durationMs ?? LIVE_ANOMALY_DEFAULTS.connectionDrop.durationMs;
      this.#dropLastReal = performance.now();
      this.#dropEndReal = this.#dropLastReal + durationMs;
      host.stall({ reason: "connection-lost", detail: "injected drop" });
      return;
    }

    // Anchor the window at the core's current run clock: the same absolute
    // elapsed the synthesis hooks match anomalies against.
    const elapsed = host.elapsed;
    const defaults =
      a.kind === "latency-spike"
        ? LIVE_ANOMALY_DEFAULTS.latencySpike
        : a.kind === "packet-loss"
          ? LIVE_ANOMALY_DEFAULTS.packetLoss
          : LIVE_ANOMALY_DEFAULTS.throughputDrop;
    const durationMs = a.durationMs ?? defaults.durationMs;
    this.#liveAnomalies.push({
      kind: a.kind,
      start: elapsed,
      end: elapsed + durationMs,
      magnitude: a.magnitude ?? defaults.magnitude,
    });
  }

  /** The currently-active live anomaly of a given kind, if any. Also prunes
   *  windows that have fully elapsed so the list stays bounded. */
  #activeAnomaly(
    kind: RunnerAnomaly["kind"],
    elapsed: number,
  ): LiveAnomaly | null {
    if (this.#liveAnomalies.length) {
      this.#liveAnomalies = this.#liveAnomalies.filter((x) => elapsed < x.end);
    }
    for (const x of this.#liveAnomalies) {
      if (x.kind === kind && elapsed >= x.start && elapsed < x.end) return x;
    }
    return null;
  }
}
