// The two ping channels: the stage-scoped latency channel and the persistent
// idle keepalive. The ping worker owns its WebSocket, its reconnects and the
// RTT timestamps; these classes own the worker's lifecycle and route its
// samples into the core.
import type { CoreHost } from "../core";
import type { PingCadence, TransportKind } from "../contract";
import type {
  FetchThroughputTarget,
  WebSocketLatencyTarget,
} from "../../api/endpoints";
import { authEnabled, redirectToLogin } from "../../auth";
import { httpToWs, throughputTargetKey } from "./backendPure";
import { pingWorker, stopWorker, type AuthRequiredMsg } from "./workerPool";
import { TransportUnavailableError } from "./transportError";

/** One measured ping the worker reports (rtt already computed in-worker). */
interface PingSample {
  rtt: number;
  lost: boolean;
}

/** Ping worker → channel messages. The worker owns reconnection, so it emits
 *  stall/resume around a reconnect window rather than a terminal error. */
type PingOutMsg =
  | { type: "open" }
  | { type: "ready" }
  | { type: "samples"; samples: PingSample[] }
  | { type: "stall"; detail: string }
  | { type: "resume" }
  | AuthRequiredMsg;

// Ping pacing is separate for idle, latency, and loaded-transfer contexts.
const PING_LOSS_K = 4;
const PING_LOSS_FLOOR_MS = 250;
const FIXED_PING_INTERVAL: Record<
  Exclude<PingCadence, "reply-driven">,
  number
> = {
  fast: 80,
  medium: 250,
  slow: 600,
};
const PING_MAX_IN_FLIGHT = 16;
const PING_REPLY_MAX_IN_FLIGHT = 4;
const PING_LOADED_MAX_IN_FLIGHT = 2;
const PING_REPORT_GAP_MS = 20;
const PING_ESTABLISH_TIMEOUT_MS = 3500;

// One low-rate idle ping worker powers connectivity and preflight RTT outside runs.
const IDLE_PING_INTERVAL_MS = 1000;
const PROBE_PING_INTERVAL_MS = 120;
const PROBE_PING_COUNT = 5;
const PROBE_PING_TIMEOUT_MS = 1500;
const IDLE_RESPAWN_MS = 2000;

export interface LatencyChannelDeps {
  host: () => CoreHost;
  target: () => WebSocketLatencyTarget | null;
  /** Stall/resume reported by the ping worker, for the coordinator to reconcile
   *  with the stage-level flag the byte lanes also drive. */
  stall: (detail: string) => void;
  resume: () => void;
}

/** The stage-owned ping channel: one per stage (idle latency, then each loaded
 *  transfer stage). It runs on its OWN socket, never on the stage's transfer
 *  transport. */
export class LatencyChannel {
  #deps: LatencyChannelDeps;
  #worker: Worker | null = null;
  /** True from prime to teardown — gates late worker messages after teardown. */
  #active = false;
  /** Armed for the idle latency stage only: fires failStage("latency") when no
   *  pong ever arrives. Cleared by the first sample / teardown. */
  #establishTimer: ReturnType<typeof setTimeout> | null = null;
  /** The underLoad tag stamped on forwarded samples (true during a transfer
   *  stage's loaded latency). Set when measure() flips reporting on. */
  #underLoad = false;

  constructor(deps: LatencyChannelDeps) {
    this.#deps = deps;
  }

  /** Open the latency (ping) channel over `kind` and warm it. Spawns the
   *  dedicated ping worker (which owns the WebSocket + the whole ping algorithm),
   *  hands it the tuning, and lets it send warmup pings — pushing NOTHING into
   *  the core. measure() flips reporting on over the SAME warmed socket. */
  prime(kind: TransportKind, isLatencyStage = false): void {
    if (kind !== "websocket") throw new Error(`unsupported ${kind}`);

    const host = this.#deps.host();
    const cfg = host.config!;
    const channel = this.#deps.target();
    const latencyRoute = channel?.routes.ping;
    if (!channel || channel.transport !== "websocket" || !latencyRoute)
      throw new Error("latency target not resolved");
    const url = httpToWs(channel.origin) + latencyRoute;
    const cadence = isLatencyStage ? cfg.pingCadence : cfg.loadedPingCadence;
    const replyDriven = cadence === "reply-driven";
    // Reply-driven uses this only for its loss sweep; its sends are driven by
    // PONGs and the worker's adaptive backup.
    const intervalMs = replyDriven
      ? PING_LOSS_FLOOR_MS
      : FIXED_PING_INTERVAL[cadence];

    this.#underLoad = false;
    this.#active = true;
    // The idle latency stage has no byte lanes to prove the link — bound how
    // long the channel gets to deliver its first pong before the stage skips.
    if (isLatencyStage) {
      this.#establishTimer = setTimeout(() => {
        this.#establishTimer = null;
        host.failStage(
          "latency",
          "connection-lost",
          "ping connection could not be established",
        );
      }, PING_ESTABLISH_TIMEOUT_MS);
    }
    const worker = pingWorker();
    worker.onmessage = (e: MessageEvent<PingOutMsg>): void =>
      this.#onMessage(e.data);
    worker.onerror = (e: ErrorEvent): void =>
      this.#onMessage({
        type: "stall",
        detail: e.message || "ping worker error",
      });
    worker.postMessage({
      type: "start",
      url,
      intervalMs,
      replyDriven,
      maxInFlight: replyDriven
        ? Math.min(
            PING_REPLY_MAX_IN_FLIGHT,
            isLatencyStage ? PING_MAX_IN_FLIGHT : PING_LOADED_MAX_IN_FLIGHT,
          )
        : isLatencyStage
          ? PING_MAX_IN_FLIGHT
          : PING_LOADED_MAX_IN_FLIGHT,
      reportGapMs: PING_REPORT_GAP_MS,
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
      checkAuthentication: authEnabled,
    });
    this.#worker = worker;
  }

  /** Begin measuring on the already-open ping channel (opened in prime()).
   *  RTT = now − sent; an unacked / timed-out ping is `lost`. The channel
   *  retains the cadence selected when its stage warmup began; measurement only
   *  enables reporting via host.ingestLatency(rtt, underLoad, lost) —
   *  `underLoad` is true when the pings run concurrently with a transfer
   *  (bufferbloat). */
  measure(underLoad: boolean): void {
    // The worker primed in prime() is already pinging on a warm socket; just
    // flip reporting on (never re-spawn — that would throw away the warmup).
    this.#underLoad = underLoad;
    this.#worker?.postMessage({ type: "measure" });
  }

  /** Stop + terminate the ping worker (closes its WebSocket). Idempotent. */
  teardown(): void {
    this.#active = false;
    this.#clearEstablishTimer();
    if (this.#worker) {
      stopWorker(this.#worker);
      this.#worker = null;
    }
  }

  /** Handle a message from the ping worker. The worker reports already-computed
   *  RTTs; the channel just tags underLoad and forwards. stall/resume bracket a
   *  reconnect — the coordinator decides whether that reaches the core. */
  #onMessage(msg: PingOutMsg): void {
    if (!this.#active) return; // late message after teardown
    if (msg.type === "auth-required") {
      this.teardown();
      redirectToLogin();
      return;
    }
    switch (msg.type) {
      case "samples": {
        this.#clearEstablishTimer(); // a pong proves the channel works
        const host = this.#deps.host();
        for (const sample of msg.samples)
          host.ingestLatency(sample.rtt, this.#underLoad, sample.lost);
        break;
      }
      case "stall":
        this.#deps.stall(msg.detail);
        break;
      case "resume":
        this.#deps.resume();
        break;
      case "open":
      case "ready":
        break;
    }
  }

  #clearEstablishTimer(): void {
    if (this.#establishTimer) {
      clearTimeout(this.#establishTimer);
      this.#establishTimer = null;
    }
  }
}

export interface IdleKeepaliveDeps {
  host: () => CoreHost;
  throughputTarget: () => FetchThroughputTarget | null;
  latencyTarget: () => WebSocketLatencyTarget | null;
}

/** The persistent idle ping: connectivity indicator plus the preflight RTT
 *  median. Separate from the stage-scoped LatencyChannel and never active at
 *  the same time (stopped when a run starts, restarted when it ends). */
export class IdleKeepalive {
  #deps: IdleKeepaliveDeps;
  #worker: Worker | null = null;
  #active = false;
  #targetKey = "";
  /** Set while collectRtts() is harvesting the keepalive's first RTTs; `finish`
   *  resolves the preflight median wait (idempotent). */
  #probeCollect: { rtts: number[]; finish: () => void } | null = null;
  #probeReady: { finish: (error?: Error) => void } | null = null;
  /** True after the keepalive reported a stall, so "connected" is emitted only
   *  on the offline→online edge instead of once per sample. */
  #offline = false;
  /** Pending respawn of a dead idle worker (script failed to load / crashed);
   *  see IDLE_RESPAWN_MS. Cleared on stop. */
  #respawnTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(deps: IdleKeepaliveDeps) {
    this.#deps = deps;
  }

  /** Start the persistent idle ping at `intervalMs` (default 1/s). Safe to
   *  call repeatedly — no-ops if already running or if websocket isn't
   *  available. Started by probe() (at the brisk preflight cadence) and again
   *  after every run ends, so the connectivity pill stays live whenever the app
   *  isn't mid-test. It uses a tiny in-flight window and a fixed internal
   *  cadence. */
  start(intervalMs = IDLE_PING_INTERVAL_MS): void {
    const targetKey = `${throughputTargetKey(this.#deps.throughputTarget())}\n${this.#deps.latencyTarget()?.id ?? ""}`;
    if (this.#active && this.#targetKey === targetKey) return;
    if (this.#active) this.stop();
    const channel = this.#deps.latencyTarget();
    const latencyRoute = channel?.routes.ping;
    if (!channel || channel.transport !== "websocket" || !latencyRoute) return;
    const url = httpToWs(channel.origin) + latencyRoute;
    this.#active = true;
    this.#targetKey = targetKey;
    // Treat connectivity as unknown until this (fresh) worker proves the link:
    // its first samples then emit a "connected" edge. Crucial after a
    // connection-lost failure — the store latched the pulse offline, and
    // without this edge a link that recovered before the worker's first stall
    // would never un-latch it.
    this.#offline = true;
    const worker = pingWorker();
    worker.onmessage = (e: MessageEvent<PingOutMsg>): void =>
      this.#onMessage(e.data);
    worker.onerror = (e: ErrorEvent): void => {
      // Worker died without ever running its reconnect loop — most commonly the
      // script fetch itself failed because the (bundle-serving) server is down,
      // e.g. restarting the keepalive right after a connection-lost run. Report
      // offline and retry the SPAWN until one sticks (the in-worker reconnect
      // loop only exists once the script loads).
      this.#onMessage({
        type: "stall",
        detail: e.message || "idle ping worker error",
      });
      this.#scheduleRespawn(intervalMs);
    };
    worker.postMessage({
      type: "start",
      url,
      intervalMs,
      replyDriven: false,
      maxInFlight: 2,
      reportGapMs: 0, // paced sends are already sparse — report every sample
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
      checkAuthentication: authEnabled,
    });
    // Report immediately (there is no keepalive warmup window).
    worker.postMessage({ type: "measure" });
    this.#worker = worker;
  }

  /** Stop the idle keepalive — a real run is starting (onRunStart), or the
   *  app is tearing down. Idempotent. */
  stop(): void {
    this.#active = false;
    this.#targetKey = "";
    if (this.#respawnTimer) {
      clearTimeout(this.#respawnTimer);
      this.#respawnTimer = null;
    }
    this.#probeCollect?.finish();
    this.#probeReady?.finish(
      new TransportUnavailableError("latency WebSocket validation stopped", {
        role: "latency",
      }),
    );
    if (this.#worker) {
      stopWorker(this.#worker);
      this.#worker = null;
    }
  }

  /** Start the idle keepalive at the brisk probe cadence and resolve with its
   *  first PROBE_PING_COUNT RTTs (median → preTestPingMs), then settle the
   *  worker to the sparse liveness cadence. Best-effort: resolves with whatever it
   *  gathered by the timeout, never rejects. */
  collectRtts(signal?: AbortSignal): Promise<number[]> {
    if (signal?.aborted) return Promise.resolve([]);
    this.start(PROBE_PING_INTERVAL_MS);
    if (!this.#worker) return Promise.resolve([]);
    return new Promise<number[]>((resolve) => {
      const finish = (): void => {
        if (!this.#probeCollect) return;
        clearTimeout(timer);
        signal?.removeEventListener("abort", finish);
        const rtts = this.#probeCollect.rtts;
        this.#probeCollect = null;
        this.#worker?.postMessage({
          type: "measure",
          intervalMs: IDLE_PING_INTERVAL_MS,
        });
        resolve(rtts);
      };
      const timer = setTimeout(finish, PROBE_PING_TIMEOUT_MS);
      signal?.addEventListener("abort", finish, { once: true });
      this.#probeCollect = { rtts: [], finish };
    });
  }

  /** Resolve once the keepalive worker reports its socket ready, so a run can
   *  refuse to start on a latency transport that never establishes. Rejects with
   *  TransportUnavailableError on timeout or if the keepalive is stopped. */
  verifyReady(signal?: AbortSignal): Promise<void> {
    if (signal?.aborted) return Promise.reject(signal.reason);
    this.start(PROBE_PING_INTERVAL_MS);
    return new Promise<void>((resolve, reject) => {
      const finish = (error?: Error): void => {
        clearTimeout(timer);
        signal?.removeEventListener("abort", aborted);
        this.#probeReady = null;
        if (error) reject(error);
        else resolve();
      };
      const aborted = (): void =>
        finish(new Error("latency WebSocket validation aborted"));
      const timer = setTimeout(
        () =>
          finish(
            new TransportUnavailableError(
              "latency WebSocket did not become ready",
              { role: "latency" },
            ),
          ),
        PROBE_PING_TIMEOUT_MS,
      );
      this.#probeReady = { finish };
      signal?.addEventListener("abort", aborted, { once: true });
    });
  }

  /** Re-spawn the idle worker after it died at load time (see the onerror
   *  handler in start()). One timer at a time; each failed attempt schedules the
   *  next, so the keepalive keeps knocking every IDLE_RESPAWN_MS until the
   *  server is back to serve the script. */
  #scheduleRespawn(intervalMs?: number): void {
    if (!this.#active || this.#respawnTimer) return;
    this.#respawnTimer = setTimeout(() => {
      this.#respawnTimer = null;
      if (!this.#active) return; // a run started (or teardown) meanwhile
      this.stop();
      this.start(intervalMs);
    }, IDLE_RESPAWN_MS);
  }

  /** Handle a message from the idle ping worker. Idle samples never reach
   *  `host.ingestLatency` (run accumulation) — they are emitted as raw
   *  `latency` events tagged phase "idle", which the store routes to its
   *  keepalive-only buffer; the `connectivity` event is the hard override
   *  effectiveConnectivity respects (stall()/resume() no-op outside a run). */
  #onMessage(msg: PingOutMsg): void {
    if (!this.#active) return;
    if (msg.type === "auth-required") {
      this.stop();
      redirectToLogin();
      return;
    }
    const host = this.#deps.host();
    switch (msg.type) {
      case "samples":
        for (const sample of msg.samples) {
          if (this.#probeCollect && !sample.lost) {
            this.#probeCollect.rtts.push(sample.rtt);
            if (this.#probeCollect.rtts.length >= PROBE_PING_COUNT)
              this.#probeCollect.finish();
          }
          host.emit({
            type: "latency",
            sample: {
              t: 0,
              rttMs: sample.rtt,
              underLoad: false,
              lost: sample.lost,
              phase: "idle",
            },
          });
        }
        if (this.#offline) {
          this.#offline = false;
          host.emit({ type: "connectivity", state: "connected" });
        }
        break;
      case "stall":
        this.#offline = true;
        host.emit({ type: "connectivity", state: "offline" });
        break;
      case "resume":
      case "open":
        break;
      case "ready":
        this.#probeReady?.finish();
        break;
    }
  }
}
