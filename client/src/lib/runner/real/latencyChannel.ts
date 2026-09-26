// Own ping-worker lifetime and route observations into the active stage or idle view.
import type { ParticipantHost } from "../transport";
import type {
  ConnectivityState,
  LatencyBucket,
  PingCadence,
} from "../contract";
import type { LatencyTarget } from "../../api/endpoints";
import { authEnabled } from "../../auth";
import {
  socketMint,
  reportServerAuthentication,
  ServerAuthenticationRequired,
  type ServerCredentials,
} from "../../servers/credentials";
import { httpToWs, ROUTES } from "../paths";
import { ESTABLISH_BUDGET_MS, ESTABLISH_MARGIN_MS } from "./budgets";
import { singleLatencyBucket } from "../series";
import { fixedPingIntervalMs } from "../pingCadence";
import {
  pingSampleContextTime,
  PING_STOP_MARGIN_MS,
  PING_TIMEOUT_CEIL_MS,
  type PingWorkerEvent,
} from "../workers/pingSample";

// Ping pacing is separate for idle, latency, and loaded-transfer contexts.
const PROBE_DEADLINE_K = 4;
const PROBE_DEADLINE_FLOOR_MS = 250;
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

/** Starts a ping worker on the target's bus; `failed` hears a worker that cannot load. */
function startPingWorker(
  target: LatencyTarget,
  credentials: ServerCredentials | undefined,
  pacing: { intervalMs: number; replyDriven: boolean; maxInFlight: number },
  on: (msg: PingWorkerEvent) => void,
  failed: (detail: string) => void,
): Worker {
  const wt = target.transport === "webtransport";
  const route = wt ? ROUTES.wtPing : ROUTES.ping;
  const worker = new Worker(
    new URL("../workers/ping-worker.ts", import.meta.url),
    { type: "module" },
  );
  worker.onmessage = (e: MessageEvent<PingWorkerEvent>) => on(e.data);
  worker.onerror = (e: ErrorEvent) => failed(e.message || "ping worker error");
  worker.postMessage({
    type: "start",
    url: (wt ? target.origin : httpToWs(target.origin)) + route,
    transport: target.transport,
    mint: socketMint(credentials, target.origin, route, wt ? "wt" : "ws"),
    ...pacing,
    deadlineK: PROBE_DEADLINE_K,
    deadlineFloorMs: PROBE_DEADLINE_FLOOR_MS,
    checkAuthentication: credentials
      ? credentials.kind === "session"
      : authEnabled(),
  });
  return worker;
}

interface LatencyChannelDeps {
  credentials?: ServerCredentials;
  host: Pick<
    ParticipantHost,
    | "latency"
    | "latencyInterrupted"
    | "latencyIncomplete"
    | "stallLatency"
    | "resumeLatency"
    | "authenticationRequired"
  >;
  target: LatencyTarget;
  /** Window-realm performance origin. Injectable only for deterministic cross-realm timestamp tests. */
  timeOriginMs?: number;
}
export type IdleEvent =
  | { type: "latency"; sample: LatencyBucket }
  | { type: "connectivity"; state: ConnectivityState };

/* The stage-owned ping channel: one per stage (idle latency, then each loaded transfer stage). */
export class LatencyChannel {
  #deps: LatencyChannelDeps;
  #worker: Worker | null = null;
  /** True from prime to teardown. Gates late worker messages. */
  #active = false;
  #ready = false;
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
  prime(cadence: PingCadence, isLatencyStage = false): void {
    this.teardown();
    const fixedIntervalMs = fixedPingIntervalMs(cadence);
    const replyDriven = fixedIntervalMs == null;
    // Reply-driven uses this only for its deadline sweep; PONGs and the adaptive backup drive its sends.
    const intervalMs = fixedIntervalMs ?? PROBE_DEADLINE_FLOOR_MS;
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
      this.#deps.host.stallLatency("ping connection could not be established");
    }, PING_ESTABLISH_TIMEOUT_MS);
    const worker: Worker = startPingWorker(
      this.#deps.target,
      this.#deps.credentials,
      { intervalMs, replyDriven, maxInFlight },
      (msg) => {
        if (this.#worker === worker) this.#onMessage(msg);
      },
      (detail) => {
        if (this.#worker !== worker) return;
        this.#deps.host.latencyIncomplete();
        this.#onMessage({ type: "stall", detail });
        if (this.#worker === worker) this.teardown();
      },
    );
    this.#worker = worker;
  }

  /* The worker owns RTT, deadlines and observation time; this channel translates only the cross-realm clock. */
  measure(): void {
    this.#worker?.postMessage({ type: "measure" });
  }

  get ready(): boolean {
    return this.#active && this.#ready;
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
      this.#deps.host.latencyIncomplete();
      this.#deps.host.stallLatency(
        "ping worker did not finish its pending probes",
      );
      if (this.#worker === worker) this.teardown();
    }, PING_TIMEOUT_CEIL_MS + PING_STOP_MARGIN_MS);
    this.#finishing = { promise, resolve, timer };
    try {
      worker.postMessage({ type: "stop", cutoffEpochMs: this.#cutoffEpochMs });
    } catch {
      this.#deps.host.latencyIncomplete();
      this.#deps.host.stallLatency(
        "ping worker could not finalize its pending probes",
      );
      if (this.#worker === worker) this.teardown();
    }
    return promise;
  }

  /** Hard stage failure cannot establish which buffered or pending outcomes were discarded. */
  discard(): void {
    if (this.#worker) this.#deps.host.latencyIncomplete();
    this.teardown();
  }

  /* Terminating the ping worker also releases its transport. */
  teardown(): void {
    this.#active = false;
    this.#ready = false;
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
      reportServerAuthentication(
        this.#deps.credentials,
        this.#deps.host,
        "latency",
      );
      this.#deps.host.latencyIncomplete();
      this.#deps.host.stallLatency("Sign in again to measure latency");
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
          host.latency({
            rttMs: sample.rtt,
            reflectorHandlingMs: sample.reflectorHandlingMs,
            timedOut: sample.timedOut,
            observedAtMs: pingSampleContextTime(sample, this.#timeOriginMs),
            rttEligible:
              this.#cutoffEpochMs === null ||
              sample.observedAtEpochMs <= this.#cutoffEpochMs,
          });
        }
        if (!this.#finishing && msg.samples.some((sample) => !sample.timedOut))
          this.#deps.host.resumeLatency();
        break;
      }
      case "interrupted":
        this.#deps.host.latencyInterrupted(
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
        this.#ready = false;
        this.#deps.host.stallLatency(msg.detail);
        break;
      case "resume":
        // Socket establishment alone does not restore latency evidence.
        break;
      case "ready":
        this.#ready = true;
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
  #emit: (event: IdleEvent) => void = () => {};
  set onEvent(handler: (event: IdleEvent) => void) {
    this.#emit = handler;
    if (this.#active && this.#connectivity)
      handler({ type: "connectivity", state: this.#connectivity });
  }
  get onEvent(): (event: IdleEvent) => void {
    return this.#emit;
  }
  #target: LatencyTarget;
  #credentials?: ServerCredentials;
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

  constructor(
    target: LatencyTarget,
    timeOriginMs = performance.timeOrigin,
    credentials?: ServerCredentials,
  ) {
    this.#target = target;
    this.#credentials = credentials;
    this.#timeOriginMs = timeOriginMs;
  }

  /* Start the persistent idle ping at `intervalMs`. */
  start(intervalMs = IDLE_PING_INTERVAL_MS): void {
    if (this.#active) return;
    this.#active = true;
    this.#connectivity = null;
    const worker: Worker = startPingWorker(
      this.#target,
      this.#credentials,
      { intervalMs, replyDriven: false, maxInFlight: 2 },
      (msg) => {
        if (this.#worker === worker) this.#onMessage(msg);
      },
      (detail) => {
        if (this.#worker !== worker) return;
        // A worker that dies at load time has no reconnect loop of its own.
        this.#onMessage({ type: "stall", detail });
        this.#scheduleRespawn(intervalMs);
      },
    );
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
    this.#probeReady?.finish(new Error("latency channel validation stopped"));
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
        () => finish(new Error("latency channel did not become ready")),
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
      if (this.#credentials?.kind === "grant")
        this.#probeReady?.finish(
          new ServerAuthenticationRequired(this.#credentials.server),
        );
      this.stop();
      reportServerAuthentication(this.#credentials);
      return;
    }
    switch (msg.type) {
      case "samples": {
        let receivedPong = false;
        for (const sample of msg.samples) {
          if (this.#probeCollect && !sample.timedOut) {
            this.#probeCollect.rtts.push(sample.rtt);
            if (this.#probeCollect.rtts.length >= PROBE_PING_COUNT)
              this.#probeCollect.finish();
          }
          if (!sample.timedOut) receivedPong = true;
          this.onEvent({
            type: "latency",
            sample: singleLatencyBucket(
              pingSampleContextTime(sample, this.#timeOriginMs),
              sample.rtt,
              sample.timedOut,
            ),
          });
        }
        // A timeout-only batch proves the worker is running, not that the server answered; recover only after a pong.
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
