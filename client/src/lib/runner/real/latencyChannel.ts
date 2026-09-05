// Own ping-worker lifetime and route observations into the active stage or idle view.
import type { CoreHost } from "../core";
import type { RunnerEvent, TransportKind } from "../contract";
import type { LatencyTarget } from "../../api/endpoints";
import { authEnabled, csrfHeader, redirectToLogin } from "../../auth";
import { httpToWs } from "./backendPure";
import { pingWorker } from "./workerPool";
import { TransportUnavailableError } from "./transportError";
import { ESTABLISH_BUDGET_MS, ESTABLISH_MARGIN_MS } from "./budgets";
import { singleLatencyBucket } from "../latencyBuckets";
import { fixedPingIntervalMs } from "../pingCadence";
import {
  pingSampleContextTime,
  PING_STOP_MARGIN_MS,
  PING_TIMEOUT_CEIL_MS,
  type PingWorkerEvent,
} from "../workers/pingSample";

// Ping pacing is separate for idle, latency, and loaded-transfer contexts.
const PING_LOSS_K = 4;
const PING_LOSS_FLOOR_MS = 250;
const PING_MAX_IN_FLIGHT = 16;
const PING_REPLY_MAX_IN_FLIGHT = 4;
const PING_LOADED_MAX_IN_FLIGHT = 2;
const PING_ESTABLISH_TIMEOUT_MS = ESTABLISH_BUDGET_MS + ESTABLISH_MARGIN_MS;

// One low-rate idle ping worker powers connectivity and preflight RTT outside runs.
const IDLE_PING_INTERVAL_MS = 1000;
const PROBE_PING_INTERVAL_MS = 120;
const PROBE_PING_COUNT = 5;
/** How long the probe waits for its RTT samples once the bus is up. */
const PROBE_PING_TIMEOUT_MS = 1500;
const IDLE_RESPAWN_MS = 2000;

/* The bus URL for a target, or null when the target does not speak `kind`. */
function pingUrl(
  target: LatencyTarget | null,
  kind: TransportKind,
): string | null {
  if (!target || target.transport !== kind) return null;
  return target.transport === "webtransport"
    ? target.origin + target.routes.wtPing
    : httpToWs(target.origin) + target.routes.ping;
}

/* Token mint for a WebTransport ping dial, when authentication is on. */
function pingMint(target: LatencyTarget | null):
  | {
      url: string;
      headers?: Record<string, string>;
      credentials?: RequestCredentials;
    }
  | undefined {
  if (!authEnabled || target?.transport !== "webtransport") return undefined;
  return {
    url: target.origin + target.routes.wtSession,
    headers: csrfHeader(),
    credentials: "include",
  };
}

interface LatencyChannelDeps {
  host: CoreHost;
  target: LatencyTarget;
  /* Reconnect edges reported by the ping worker. */
  stall: (detail: string) => void;
  resume: () => void;
  /** Window-realm performance origin. Injectable only for deterministic cross-realm timestamp tests. */
  timeOriginMs?: number;
}

/* The stage-owned ping channel: one per stage (idle latency, then each loaded transfer stage). */
export class LatencyChannel {
  #deps: LatencyChannelDeps;
  #worker: Worker | null = null;
  /** True from prime to teardown. Gates late worker messages. */
  #active = false;
  /* A timeout reports a stall; the runner owns whether the latency stage eventually expires. */
  #establishTimer: ReturnType<typeof setTimeout> | null = null;
  #timeOriginMs: number;
  #cutoffEpochMs: number | null = null;
  #finishing: {
    promise: Promise<void>;
    resolve: () => void;
    timer: ReturnType<typeof setTimeout>;
  } | null = null;

  constructor(deps: LatencyChannelDeps) {
    this.#deps = deps;
    this.#timeOriginMs = deps.timeOriginMs ?? performance.timeOrigin;
  }

  /* The ping worker owns the bus and the ping algorithm. */
  prime(isLatencyStage = false): void {
    this.teardown();
    const host = this.#deps.host;
    const cfg = host.config!;
    const channel = this.#deps.target;
    const kind = channel.transport;
    const url = pingUrl(channel, kind);
    if (!url) throw new Error("latency target not resolved");
    const cadence = isLatencyStage ? cfg.pingCadence : cfg.loadedPingCadence;
    const fixedIntervalMs = fixedPingIntervalMs(cadence);
    const replyDriven = fixedIntervalMs == null;
    // Reply-driven uses this only for its loss sweep; its sends are driven by PONGs and the worker's adaptive backup.
    const intervalMs = fixedIntervalMs ?? PING_LOSS_FLOOR_MS;
    // A loaded stage shares the link with the transfer, so its depth is the same either way; the idle stage goes.
    const maxInFlight = !isLatencyStage
      ? PING_LOADED_MAX_IN_FLIGHT
      : replyDriven
        ? PING_REPLY_MAX_IN_FLIGHT
        : PING_MAX_IN_FLIGHT;

    this.#cutoffEpochMs = null;
    this.#active = true;
    // A bus that never establishes reports nothing at all — a hung handshake produces no samples and no stall — so.
    this.#establishTimer = setTimeout(() => {
      this.#establishTimer = null;
      const detail = "ping connection could not be established";
      this.#deps.stall(detail);
    }, PING_ESTABLISH_TIMEOUT_MS);
    const worker = pingWorker();
    worker.onmessage = (e: MessageEvent<PingWorkerEvent>): void => {
      if (this.#worker === worker) this.#onMessage(e.data);
    };
    worker.onerror = (e: ErrorEvent): void => {
      if (this.#worker !== worker) return;
      this.#deps.host.ingestLatencyAccountingIncomplete();
      this.#onMessage({
        type: "stall",
        detail: e.message || "ping worker error",
      });
      if (this.#worker === worker) this.teardown();
    };
    this.#worker = worker;
    worker.postMessage({
      type: "start",
      url,
      transport: kind,
      mint: pingMint(channel),
      intervalMs,
      replyDriven,
      maxInFlight,
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
      checkAuthentication: authEnabled,
    });
  }

  /* The worker owns RTT, loss, and observation time; this channel translates only the cross-realm clock coordinate. */
  measure(): void {
    this.#worker?.postMessage({ type: "measure" });
  }

  /** Stop sends at this clock boundary, then admit terminal outcomes before terminating the worker. */
  finish(): Promise<void> {
    if (this.#finishing) return this.#finishing.promise;
    if (!this.#worker) return Promise.resolve();
    const worker = this.#worker;
    this.#clearEstablishTimer();
    this.#cutoffEpochMs = this.#timeOriginMs + performance.now();
    let resolve!: () => void;
    const promise = new Promise<void>((done) => {
      resolve = done;
    });
    const timer = setTimeout(() => {
      if (this.#worker !== worker) return;
      this.#deps.host.ingestLatencyAccountingIncomplete();
      this.#deps.stall("ping worker did not finish its pending probes");
      if (this.#worker === worker) this.teardown();
    }, PING_TIMEOUT_CEIL_MS + PING_STOP_MARGIN_MS);
    this.#finishing = { promise, resolve, timer };
    try {
      worker.postMessage({ type: "stop", cutoffEpochMs: this.#cutoffEpochMs });
    } catch {
      this.#deps.host.ingestLatencyAccountingIncomplete();
      this.#deps.stall("ping worker could not finalize its pending probes");
      if (this.#worker === worker) this.teardown();
    }
    return promise;
  }

  /** Hard stage failure cannot establish which buffered or pending outcomes were discarded. */
  discard(): void {
    if (this.#worker) this.#deps.host.ingestLatencyAccountingIncomplete();
    this.teardown();
  }

  /* Terminating the ping worker also releases its transport. */
  teardown(): void {
    this.#active = false;
    this.#clearEstablishTimer();
    if (this.#worker) {
      this.#worker.terminate();
      this.#worker = null;
    }
    if (this.#finishing) {
      clearTimeout(this.#finishing.timer);
      this.#finishing.resolve();
      this.#finishing = null;
    }
  }

  /* Handle a message from the ping worker. */
  #onMessage(msg: PingWorkerEvent): void {
    if (!this.#active) return; // late message after teardown
    if (msg.type === "auth-required") {
      this.teardown();
      redirectToLogin();
      return;
    }
    switch (msg.type) {
      case "samples": {
        this.#clearEstablishTimer(); // a pong proves the channel works
        const host = this.#deps.host;
        for (const sample of msg.samples) {
          if (
            this.#cutoffEpochMs !== null &&
            (sample.sentAtEpochMs ?? -Infinity) > this.#cutoffEpochMs
          )
            continue;
          host.ingestLatency({
            rttMs: sample.rtt,
            lost: sample.lost,
            observedAtMs: pingSampleContextTime(sample, this.#timeOriginMs),
            rttEligible:
              this.#cutoffEpochMs === null ||
              sample.observedAtEpochMs <= this.#cutoffEpochMs,
          });
        }
        if (!this.#finishing && msg.samples.some((sample) => !sample.lost))
          this.#deps.resume();
        break;
      }
      case "interrupted":
        this.#deps.host.ingestLatencyInterruption(
          msg.sentAtEpochMs.filter(
            (sentAt) =>
              this.#cutoffEpochMs === null || sentAt <= this.#cutoffEpochMs,
          ).length,
          msg.reason,
        );
        break;
      case "stopped":
        this.teardown();
        break;
      case "stall":
        this.#deps.stall(msg.detail);
        break;
      case "resume":
        // Socket establishment alone does not restore latency evidence.
        break;
      case "ready":
        // Warmup pongs stay in the worker, so waiting for a measured sample can outlive warmup.
        this.#clearEstablishTimer();
        break;
      case "open":
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

// Idle monitoring owns a separate worker, stopped before any measured run.
export class IdleKeepalive {
  #emit: (event: RunnerEvent) => void = () => {};
  set onEvent(handler: (event: RunnerEvent) => void) {
    this.#emit = handler;
    if (this.#active && this.#connectivity)
      handler({ type: "connectivity", state: this.#connectivity });
  }
  get onEvent(): (event: RunnerEvent) => void {
    return this.#emit;
  }
  #target: LatencyTarget;
  #timeOriginMs: number;
  #worker: Worker | null = null;
  #active = false;
  /* Set while collectRtts() is harvesting the keepalive's first RTTs; `finish` resolves the preflight median wait. */
  #probeCollect: { rtts: number[]; finish: () => void } | null = null;
  #probeReady: { finish: (error?: Error) => void } | null = null;
  /** Readiness alone is not liveness; only a pong or stall establishes connectivity. */
  #connectivity: "connected" | "offline" | null = null;
  /** Pending respawn of an idle worker that dies at load time. Cleared on stop. */
  #respawnTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(target: LatencyTarget, timeOriginMs = performance.timeOrigin) {
    this.#target = target;
    this.#timeOriginMs = timeOriginMs;
  }

  /* Start the persistent idle ping at `intervalMs`. */
  start(intervalMs = IDLE_PING_INTERVAL_MS): void {
    if (this.#active) return;
    const channel = this.#target;
    const url = pingUrl(channel, channel.transport)!;
    this.#active = true;
    this.#connectivity = null;
    const worker = pingWorker();
    worker.onmessage = (e: MessageEvent<PingWorkerEvent>): void =>
      this.#onMessage(e.data);
    worker.onerror = (e: ErrorEvent): void => {
      // A worker dying at load time has no in-worker reconnect loop, usually because the bundle-serving server is down.
      this.#onMessage({
        type: "stall",
        detail: e.message || "idle ping worker error",
      });
      this.#scheduleRespawn(intervalMs);
    };
    worker.postMessage({
      type: "start",
      url,
      transport: channel.transport,
      mint: pingMint(channel),
      intervalMs,
      replyDriven: false,
      maxInFlight: 2,
      lossK: PING_LOSS_K,
      lossFloorMs: PING_LOSS_FLOOR_MS,
      checkAuthentication: authEnabled,
    });
    // Report immediately (there is no keepalive warmup window).
    worker.postMessage({ type: "measure" });
    this.#worker = worker;
  }

  /** Stop the idle keepalive when a run starts (onRunStart) or the app tears down. Idempotent. */
  stop(): void {
    this.#active = false;
    if (this.#respawnTimer) {
      clearTimeout(this.#respawnTimer);
      this.#respawnTimer = null;
    }
    this.#probeCollect?.finish();
    this.#probeReady?.finish(
      new TransportUnavailableError("latency channel validation stopped", {
        role: "latency",
      }),
    );
    if (this.#worker) {
      this.#worker.terminate();
      this.#worker = null;
    }
  }

  /* The worker then settles to the sparse liveness cadence. */
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

  /* Resolve once the keepalive worker reports its socket ready, so a run can refuse to start on a latency transport. */
  verifyReady(signal?: AbortSignal): Promise<void> {
    if (signal?.aborted) return Promise.reject(signal.reason);
    this.start(PROBE_PING_INTERVAL_MS);
    return new Promise<void>((resolve, reject) => {
      const finish = (error?: Error): void => {
        clearTimeout(timer);
        signal?.removeEventListener("abort", aborted);
        // Only the current wait may clear its slot.
        if (this.#probeReady?.finish === finish) this.#probeReady = null;
        if (error) reject(error);
        else resolve();
      };
      const aborted = (): void =>
        finish(new Error("latency channel validation aborted"));
      const timer = setTimeout(
        () =>
          finish(
            new TransportUnavailableError(
              "latency channel did not become ready",
              { role: "latency" },
            ),
          ),
        // The worker's own establish deadline plus its mint sit inside this one, so without the margin the owner.
        PING_ESTABLISH_TIMEOUT_MS,
      );
      this.#probeReady = { finish };
      signal?.addEventListener("abort", aborted, { once: true });
    });
  }

  /* Re-spawn an idle worker that dies at load time. */
  #scheduleRespawn(intervalMs?: number): void {
    if (!this.#active || this.#respawnTimer) return;
    this.#respawnTimer = setTimeout(() => {
      this.#respawnTimer = null;
      if (!this.#active) return; // a run start or teardown clears #active
      this.stop();
      this.start(intervalMs);
    }, IDLE_RESPAWN_MS);
  }

  /* Handle a message from the idle ping worker. */
  #onMessage(msg: PingWorkerEvent): void {
    if (!this.#active) return;
    if (msg.type === "auth-required") {
      this.stop();
      redirectToLogin();
      return;
    }
    switch (msg.type) {
      case "samples": {
        let receivedPong = false;
        for (const sample of msg.samples) {
          if (this.#probeCollect && !sample.lost) {
            this.#probeCollect.rtts.push(sample.rtt);
            if (this.#probeCollect.rtts.length >= PROBE_PING_COUNT)
              this.#probeCollect.finish();
          }
          if (!sample.lost) receivedPong = true;
          this.onEvent({
            type: "latency",
            sample: singleLatencyBucket(
              pingSampleContextTime(sample, this.#timeOriginMs),
              sample.rtt,
              sample.lost,
            ),
          });
        }
        // A loss-only batch proves the worker is running, not that the server answered; recover only after a pong.
        if (receivedPong && this.#connectivity !== "connected") {
          this.#connectivity = "connected";
          this.onEvent({ type: "connectivity", state: "connected" });
        }
        break;
      }
      case "stall":
        this.#connectivity = "offline";
        this.onEvent({ type: "connectivity", state: "offline" });
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
