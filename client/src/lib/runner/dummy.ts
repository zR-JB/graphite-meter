/* ============================================================
 * The Graphite Meter — Dummy Backend (§2.2)
 * Deterministic-seedable development sample source. It implements
 * only the engine-specific part of a run — producing synthetic
 * throughput/latency samples — and pushes them into the shared
 * RunnerCore, which owns the timeline, evaluation, and event
 * stream. A real engine is the same shape with real I/O in place
 * of the synthesis below; swapping it touches only wire.ts.
 * ============================================================ */

import type {
  RunnerConfig,
  RunnerAnomaly,
  InfraInfo,
  EngineInfo,
  FlowDirection,
  PhaseActivity,
} from "./contract";
import type { CoreHost, RunnerBackend, TickContext } from "./core";
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
  /** RTT increase under load — drives the bufferbloat grade. */
  loadedDeltaMs: number;
  /** Baseline loss probability per ping. */
  lossBase: number;
  /** Relative std of the plateau (throughput and idle RTT) — how *steady* the
   *  link is. This is what the adaptive stability score reads: steady links
   *  (fiber/cable) settle to a high band and finish early on the stable window;
   *  jittery ones (lte/satellite) never settle and report the full average. */
  jitter: number;
}

// Throughput is bytes/sec (browser-native). Link rates are conventionally
// quoted in bits/sec, so the trailing comment notes the familiar bit-rate.
const PROFILES: Record<NonNullable<DummyOptions["profile"]>, ProfileSpec> = {
  fiber: { downBytesPerSec: 117.5e6, upBytesPerSec: 110e6, idleRttMs: 6, loadedDeltaMs: 4, lossBase: 0.0, jitter: 0.04 }, // ~940/880 Mbit/s
  cable: { downBytesPerSec: 40e6, upBytesPerSec: 2.75e6, idleRttMs: 16, loadedDeltaMs: 34, lossBase: 0.002, jitter: 0.05 }, // ~320/22 Mbit/s
  lte: { downBytesPerSec: 8e6, upBytesPerSec: 3e6, idleRttMs: 38, loadedDeltaMs: 62, lossBase: 0.01, jitter: 0.09 }, // ~64/24 Mbit/s
  satellite: { downBytesPerSec: 13.75e6, upBytesPerSec: 1.75e6, idleRttMs: 600, loadedDeltaMs: 180, lossBase: 0.015, jitter: 0.11 }, // ~110/14 Mbit/s
  throttled: { downBytesPerSec: 1.1875e6, upBytesPerSec: 0.5625e6, idleRttMs: 28, loadedDeltaMs: 48, lossBase: 0.005, jitter: 0.05 }, // ~9.5/4.5 Mbit/s
};

const PING_INTERVAL: Record<RunnerConfig["pingConcurrency"], number> = {
  instant: 80,
  medium: 250,
  slow: 600,
};

const THROUGHPUT_CADENCE_MS = 60; // ≈16Hz

/* ---------- Live anomaly defaults (§13.6) ----------
 * Construction-time anomalies (DummyOptions.anomalies) fire at phase fractions.
 * These are the defaults for RUNTIME anomalies injected mid-run via
 * `injectAnomaly` — each occupies an absolute [start,end) window measured in
 * ms since run start, computed from "now" when the Developer button is hit. */
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

/* ---------- Deterministic RNG (mulberry32 + Box–Muller) ---------- */
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

  // Real-time cadence gates (sim-only): throughput at ~16Hz, pings at the
  // configured interval — gated on REAL time so the early-finish glide doesn't
  // dump a whole tail's worth of samples at once.
  #lastThroughputAt = -Infinity;
  #lastPingAt = -Infinity;

  // Live, dev-injected anomalies (§13.6). Each is an absolute [start,end) window
  // on the effective timeline; the synthesis hooks read this list.
  #liveAnomalies: LiveAnomaly[] = [];

  // A live connection-drop window in REAL (wall-clock) time. While we're inside
  // it the dummy pushes NO samples — true dead air — so the core's watchdog
  // doesn't immediately auto-resume us. We stall() on inject and resume() once
  // wall-clock passes the window's end. Real-time (not measured-time) because
  // measured-time freezes during the stall, so it could never reach the end. */
  #dropEndReal = 0; // performance.now() the drop lifts at, or 0 when not dropped

  constructor(opts: DummyOptions = {}) {
    this.#opts = { profile: opts.profile ?? "fiber", ...opts };
    this.#spec = PROFILES[this.#opts.profile];
    this.#rand = mulberry32(opts.seed ?? 0x9e3779b9);
  }

  attach(host: CoreHost): void {
    this.#host = host;
  }

  /* ---------- Gaussian noise via Box–Muller ---------- */
  #gauss(): number {
    const u = Math.max(1e-9, this.#rand());
    const v = this.#rand();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  }

  /* ================= PROBE ================= */
  async probe(endpoint: RunnerConfig["endpoint"]): Promise<InfraInfo> {
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
        sample: { t: -interval * (pings - i), rttMs: rtt, underLoad: false, lost: false, phase: "idle" },
      });
    }

    const host = endpoint.host === "auto" ? "edge-fra-03.graphite.net" : endpoint.host;
    const octet = () => Math.floor(this.#rand() * 254) + 1;
    return {
      clientIp: `${octet()}.${octet()}.${octet()}.${octet()}`,
      server: {
        name: "Graphite Edge — Frankfurt",
        host,
        port: endpoint.port,
        location: "Frankfurt, DE",
      },
      preTestPingMs: this.#spec.idleRttMs,
      engineVersion: "dummy-1.0.0",
      protocolNegotiated:
        this.#opts.profile === "satellite" ? "h3 (QUIC)" : "webtransport/h3",
    };
  }

  /** Synthetic capabilities: the dummy "supports" every per-role transport,
   *  exercising the full capability surface the UI renders. Everything here is
   *  simulated. WebSocket appears only under latency — it is the ping bus,
   *  never a byte-transfer lane. */
  describe(): EngineInfo {
    return {
      name: "dummy",
      version: BUILD.clientVersion,
      latencyTransports: ["webtransport", "websocket"],
      throughputTransports: ["webtransport", "fetch-streams"],
    };
  }

  /* ================= LIFECYCLE (core → backend) ================= */
  onRunStart(_config: RunnerConfig): void {
    // Reset per-run synthesis state. The core owns the timeline + clock.
    this.#lastThroughputAt = -Infinity;
    this.#lastPingAt = -Infinity;
    this.#liveAnomalies = [];
    this.#dropEndReal = 0;
  }

  // The dummy simulates rather than opening sockets, so the stage connection
  // lifecycle is a no-op; a real backend opens/primes in onStageBegin, starts
  // measuring in onStageMeasure, and closes in onStageEnd.
  onStageBegin(_activity: PhaseActivity): void {}
  onStageMeasure(_activity: PhaseActivity): void {}
  onStageEnd(_activity: PhaseActivity): void {}
  onComplete(): void {}
  onAbort(): void {}

  /** Fallback idle RTT for an empty-sample run — the profile's idle ping. */
  idleHintMs(): number {
    return this.#spec.idleRttMs;
  }

  /* ================= PER-TICK SYNTHESIS ================= */
  onTick(ctx: TickContext): void {
    const cfg = this.#host?.config;
    if (!cfg) return;
    const { activity, isWarmup, elapsed, segStart, segEnd, realNow } = ctx;

    // Active connection-drop: true dead air — push NOTHING (so the core watchdog
    // doesn't auto-resume us), and lift the stall once wall-clock passes the
    // window end. Measured-time is frozen meanwhile, so the run end recedes by
    // exactly this real duration — the visible push-out (§4 / §drop UX).
    if (this.#dropEndReal > 0) {
      if (realNow < this.#dropEndReal) return;
      this.#dropEndReal = 0;
      this.#host!.resume();
      // Snap the sample cadence gates to "now" so resume doesn't dump a backlog.
      this.#lastThroughputAt = realNow;
      this.#lastPingAt = realNow;
    }

    // The warmup window primes real connections; the dummy has none, so it emits
    // nothing until measurement begins — mirroring a real backend, which only
    // starts pushing samples at onStageMeasure.
    if (isWarmup) return;

    // Throughput on the stage's transfer lanes (none for the latency stage; both
    // lanes for bidirectional). Cadence gated on REAL time so the early-finish
    // glide stays smooth — measured-time races ahead, and gating on it would
    // dump the tail's samples into the canvas at once (§13.4).
    if (activity.transfer.length > 0 && realNow - this.#lastThroughputAt >= THROUGHPUT_CADENCE_MS) {
      this.#lastThroughputAt = realNow;
      for (const dir of activity.transfer) {
        this.#synthThroughput(dir, elapsed, segStart, segEnd);
      }
    }

    // Pings: the idle latency stage, or loaded (bufferbloat) pings during a
    // transfer stage. `activity.loadedLatency` already folds in the "skip loaded
    // latency when the latency stage is off" rule — resolved once by the
    // scheduler, never re-derived from config here.
    const pingActive =
      activity.stage === "latency" || (activity.transfer.length > 0 && activity.loadedLatency);
    const pingInterval = PING_INTERVAL[cfg.pingConcurrency];
    if (pingActive && realNow - this.#lastPingAt >= pingInterval) {
      this.#lastPingAt = realNow;
      this.#synthLatency(activity, elapsed, segStart, segEnd);
    }
  }

  /* ---------- Throughput sample synthesis ---------- */
  // `dir` (not the phase) picks the rate, so the bidirectional phase synthesizes
  // a down lane and an up lane from the same profile per tick.
  #synthThroughput(dir: FlowDirection, elapsed: number, segStart: number, segEnd: number) {
    const tp = elapsed - segStart; // ms into this phase
    const phaseLen = segEnd - segStart;
    const mean = dir === "down" ? this.#spec.downBytesPerSec : this.#spec.upBytesPerSec;

    // Logistic ramp-up over the first ~1.2s.
    const ramp = 1 / (1 + Math.exp(-(tp - 600) / 150));

    // Noisy plateau, Gaussian noise scaled by the profile's steadiness.
    let bytesPerSec = mean * ramp * (1 + this.#gauss() * this.#spec.jitter);

    // Throughput dip anomaly: 400ms 40% drop centred on each fraction.
    for (const f of this.#opts.anomalies?.throughputDipAt ?? []) {
      const center = f * phaseLen;
      if (tp >= center && tp < center + 400) bytesPerSec *= 0.6;
    }

    // Live throughput-drop anomaly (§13.6): reduce bytesPerSec by `magnitude`
    // over its window. Fired relative to the current moment, not a fraction.
    const drop = this.#activeAnomaly("throughput-drop", elapsed);
    if (drop) bytesPerSec *= Math.max(0, 1 - drop.magnitude);

    bytesPerSec = Math.max(0, bytesPerSec);

    // Bytes transferred over the cadence window (rate is bytes/sec). The core
    // accumulates this and tracks the cumulative total. Direction travels with
    // the sample (the core never infers it from the phase).
    const bytes = bytesPerSec * (THROUGHPUT_CADENCE_MS / 1000);
    this.#host!.ingestThroughput(dir, bytesPerSec, bytes);
  }

  /* ---------- Latency sample synthesis ---------- */
  // `activity.transfer` (not the phase) decides under-load: a stage that moves
  // bytes produces loaded (bufferbloat) pings; the latency stage produces idle.
  #synthLatency(activity: PhaseActivity, elapsed: number, segStart: number, segEnd: number) {
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

    // Live latency-spike anomaly (§13.6): scale rtt by `magnitude` in-window.
    const spike = this.#activeAnomaly("latency-spike", elapsed);
    if (spike) rtt *= spike.magnitude;

    // Packet loss: baseline + burst windows.
    let lossProb = this.#spec.lossBase;
    for (const f of this.#opts.anomalies?.packetDropAt ?? []) {
      if (Math.abs(frac - f) < 0.03) lossProb = 0.6;
    }
    // Live packet-loss anomaly (§13.6): raise loss probability in-window.
    const drop = this.#activeAnomaly("packet-loss", elapsed);
    if (drop) lossProb = Math.max(lossProb, drop.magnitude);
    const lost = this.#rand() < lossProb;

    this.#host!.ingestLatency(rtt, underLoad, lost);
  }

  /* ================= LIVE ANOMALY INJECTION (§13.6) ================= */
  /**
   * Fire a dev-only anomaly into the in-flight run. It opens an absolute window
   * starting at the current elapsed and is consumed by the synthesis hooks.
   * No-op when not running (no host config), so the Developer panel's disabled
   * state mirrors `isRunning`.
   */
  injectAnomaly(a: RunnerAnomaly): void {
    const host = this.#host;
    if (!host || !host.config || host.phase === "idle") return;

    // Connection-drop is modelled as a real stall, not a synthesis tweak: stall
    // the core NOW (freezing measured-time) and open a real-time dead-air window
    // that onTick lifts with resume(). No magnitude — it's a full drop.
    if (a.kind === "connection-drop") {
      const durationMs = a.durationMs ?? LIVE_ANOMALY_DEFAULTS.connectionDrop.durationMs;
      this.#dropEndReal = performance.now() + durationMs;
      host.stall({ reason: "connection-lost", detail: "injected drop" });
      return;
    }

    // Anchor the window at the core's current run clock — the same absolute
    // elapsed the synthesis hooks match anomalies against.
    const elapsed = host.elapsed;
    const d =
      a.kind === "latency-spike"
        ? LIVE_ANOMALY_DEFAULTS.latencySpike
        : a.kind === "packet-loss"
          ? LIVE_ANOMALY_DEFAULTS.packetLoss
          : LIVE_ANOMALY_DEFAULTS.throughputDrop;
    const durationMs = a.durationMs ?? d.durationMs;
    this.#liveAnomalies.push({
      kind: a.kind,
      start: elapsed,
      end: elapsed + durationMs,
      magnitude: a.magnitude ?? d.magnitude,
    });
  }

  /** The currently-active live anomaly of a given kind, if any. Also prunes
   *  windows that have fully elapsed so the list stays bounded. */
  #activeAnomaly(kind: RunnerAnomaly["kind"], elapsed: number): LiveAnomaly | null {
    if (this.#liveAnomalies.length) {
      this.#liveAnomalies = this.#liveAnomalies.filter((x) => elapsed < x.end);
    }
    for (const x of this.#liveAnomalies) {
      if (x.kind === kind && elapsed >= x.start && elapsed < x.end) return x;
    }
    return null;
  }
}
