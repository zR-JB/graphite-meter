// The engine's shared vocabulary: servers and their verified paths, credentials, evidence, and the contract between
// the run and the resources each server opens for a stage.
import type { Probe } from "../protocol/documents";
import type { FailureReason } from "../protocol/reasons";
import type { Checkpoint } from "./measure/aggregate";
import type { Activity, Stage } from "./run/schedule";

export type { Checkpoint, FailureReason, Probe, Stage };
export type Direction = "down" | "up";
export type Role = "throughput" | "latency";
export type PingCadence = "reply-driven" | "fast" | "medium" | "slow";

/** Fixed start-to-start probe spacing; reply-driven has none. */
export const CADENCE_MS: Record<PingCadence, number | null> = {
  "reply-driven": null,
  fast: 80,
  medium: 250,
  slow: 600,
};

/** A catalogue server as results and history name it. */
export interface ServerIdentity {
  id: string;
  url: string;
  name: string;
  location?: string;
}

export type HttpProtocol = "http1" | "http2" | "http3" | "negotiated";

/** An advertised way to reach a server; `id` is what a saved selection names (the origin, `::wt` or `::wtdg`). */
export interface FetchTarget {
  id: string;
  origin: string;
  transport: "fetch-stream";
  protocol: HttpProtocol;
  tls: boolean;
}
export interface SessionTarget {
  id: string;
  origin: string;
  transport: "webtransport" | "webtransport-datagram";
  protocol: "http3";
  tls: true;
}
export type ThroughputTarget = FetchTarget | SessionTarget;
export interface LatencyTarget {
  id: string;
  origin: string;
  transport: "websocket" | "webtransport";
  protocol: "http1" | "http3";
  tls: boolean;
}

/** What one origin of a server offers this page, or why the browser cannot use it. */
export interface OriginTargets<T> {
  blocked: string | null;
  targets: T[];
}

/** A server's preflight as this page can use it. */
export interface Discovery {
  generation: string;
  engineVersion: string;
  server: { name: string; location?: string };
  uploadCheckpoint: boolean;
  /** The longest stage the server admits; 300 s for a server that predates the limit. */
  maxStageMs: number;
  /** The origin that answered the preflight, and the protocol the browser used for it. */
  origin: string;
  protocol: string | null;
  /** Monotonic page ms when it was read, and how long the request took. */
  readAt: number;
  requestMs: number;
  throughput: Map<string, OriginTargets<ThroughputTarget>>;
  latency: Map<string, OriginTargets<LatencyTarget>>;
}

/** A path proven to work, and the selection it was proven for: Automatic stays valid while its first candidate does
    not change; an explicit choice while it resolves to the verified target. */
export interface VerifiedFor {
  selection: string;
  head: string;
}
export interface ThroughputPath {
  /** What carries the bytes: a fetch target with the protocol the browser negotiated, or a WebTransport target. */
  target: ThroughputTarget;
  /** The HTTP view of the target's origin: probes, upload sessions, checkpoints and the HTTP progress feed. */
  fetch: FetchTarget;
  probe: Probe;
  /** The browser's own `nextHopProtocol`, when it reported one. */
  browserProtocol: string | null;
  generation: string;
  verifiedAt: number;
  verifiedFor: VerifiedFor;
}
export interface LatencyPath {
  target: LatencyTarget;
  probe: Probe;
  /** The median of the check's replies; null without one. */
  rttMs: number | null;
  generation: string;
  verifiedAt: number;
  verifiedFor: VerifiedFor;
}

/** A server ready to run: immutable input to one run. */
export interface PreparedServer {
  server: ServerIdentity;
  auth: ServerAuth;
  discovery: Discovery;
  throughput: ThroughputPath;
  /** Null when no planned stage probes latency. */
  latency: LatencyPath | null;
}

/** Everything a request needs, as plain data a worker can take. */
export interface RequestRecipe {
  headers: Record<string, string>;
  credentials: RequestCredentials;
  redirect: RequestRedirect;
}

/** How to mint a single-use socket ticket for one destination. */
export interface TicketRecipe {
  url: string;
  request: RequestRecipe;
}

/** A server's credentials: none, the page's own login, or a measurement grant. Requests never leave the server's
    allowed destinations, and a grant never appears in a URL. */
export interface ServerAuth {
  readonly kind: "public" | "session" | "grant";
  /** Monotonic page ms when the login or grant ends; null when it does not. */
  readonly deadline: number | null;
  /** Throws SignInRequired when the grant is missing or ended. */
  request(url: string, method: string): RequestRecipe;
  /** A control request: high priority, the recipe applied, a 403 carrying the sign-in marker thrown as
      SignInRequired with its body released. */
  fetch(url: string, init?: RequestInit): Promise<Response>;
  /** Null when a socket needs no ticket: public servers, and a WebSocket riding the page's own cookie. */
  ticket(origin: string, route: string): TicketRecipe | null;
  /** Whether a broken stream was the login or grant ending: the one cheap check per kind, at most 3 s. */
  ended(signal?: AbortSignal): Promise<boolean>;
}

export class SignInRequired extends Error {
  constructor(readonly server: ServerIdentity) {
    super(`Sign in to ${server.name}`);
  }
}

/** How a lane, feed or channel ended. `retry`: a reconnect may recover it; `rotate`: the server no longer knows the
    upload id, so the run may replace the receiver once. */
export interface Ending {
  reason: FailureReason;
  retry: boolean;
  detail: string;
  rotate?: boolean;
  retryAfterMs?: number;
}

/** A non-terminal stall: a direction went quiet or its lanes are reconnecting. */
export interface Stall {
  reason: FailureReason;
  detail: string;
  direction?: Direction;
  rotate?: boolean;
}

/** One resolved probe, timed on the page's monotonic clock. A reply after the stage's cutoff resolves its probe but
    stays out of the window's statistics (`inWindow: false`). */
export interface ProbeOutcome {
  rttMs: number;
  timedOut: boolean;
  handlingMs: number | null;
  observedAt: number;
  inWindow: boolean;
}

/** What one server's stage resources report to the run. */
export interface StageEvents {
  /** Client-consumed payload bytes of a measured download. */
  download(bytes: number): void;
  /** A receiver observation of a measured upload. */
  receiver(checkpoint: Checkpoint): void;
  probes(outcomes: readonly ProbeOutcome[]): void;
  /** Probes that will get no verdict: lost with their connection, or never sent. Neither is a timeout. */
  probesInterrupted(count: number, reason: "unresolved" | "send-failed"): void;
  /** The probe population's accounting can no longer be complete. */
  probesIncomplete(): void;
  stalled(stall: Stall): void;
  resumed(): void;
  latencyStalled(detail: string): void;
  latencyResumed(): void;
  failed(reason: FailureReason, message: string): void;
  signInRequired(role: Role): void;
  /** A locally timed upload POST: presentation evidence while a receiver pauses. */
  uploadHint(lane: number, bytes: number, elapsedMs: number): void;
}

/** A server's lanes for one stage, as the browser's connection budget dealt them. */
export type StageLanes = Pick<StageSetup, "streams" | "gating">;

export interface StageSetup {
  server: PreparedServer;
  activity: Activity;
  /** Lanes per direction for this server and stage; 0 for a direction the stage does not move. */
  streams: Record<Direction, number>;
  /** Of those, the lanes the browser can run at once, which readiness waits for (the plan's `gating`): fewer when an
      HTTP/1.1 origin's connection pool queues the rest; all of `streams` when absent. Uploads gate on their
      receiver instead. */
  gating?: Record<Direction, number>;
  /** The probe spacing of this stage: the idle cadence for the latency stage, the loaded one otherwise. */
  cadence: PingCadence;
  /** Distinguishes this run's requests from cached ones. */
  seed: string;
  events: StageEvents;
  /** Run ms, the clock checkpoints are stamped with. */
  now(): number;
}

/** One server's resources for one stage, shared by its warmup and measured window. */
export interface StageResources {
  /** Opens the carriers, receiver and probe channel, and resolves once they work: download bytes, a receiver
      checkpoint, a probe reply, within 3.5 s. Loaded latency never holds throughput back. */
  prepare(signal: AbortSignal): Promise<void>;
  /** The measured window starts: evidence from now on counts. */
  measure(): void;
  /** A receiver checkpoint; `final` issues fresh requests and retries a miss within 1.5 s. */
  checkpoint(signal: AbortSignal, final?: boolean): Promise<Checkpoint | null>;
  /** Replaces an upload id the server no longer knows: the run's one rotation per server. */
  replaceUpload(signal: AbortSignal): Promise<void>;
  /** Graceful end: probes drain to their deadlines, sessions finalize, then everything is released. */
  finish(): Promise<void>;
  /** Immediate release; `incomplete` first reports the probes it cuts off. */
  discard(incomplete?: boolean): void;
}

export type OpenStage = (setup: StageSetup) => StageResources;

export class ServerBusyError extends Error {}

/** A latency bus to one server: a WebSocket, or WebTransport datagrams, driven from a worker. */
export interface ProbeChannelOptions {
  target: LatencyTarget;
  auth: ServerAuth;
  /** Fixed start-to-start spacing in ms, or null for reply-driven probing. */
  intervalMs: number | null;
  /** Probes in flight at most; a full window skips a send, never times one out. */
  maxInFlight: number;
}
export interface ProbeChannelEvents {
  /** The first matched reply on each connection. */
  ready(): void;
  outcomes(batch: readonly ProbeOutcome[]): void;
  interrupted(count: number, reason: "unresolved" | "send-failed"): void;
  /** The connection was lost and is reconnecting (once per outage); `resumed` when replies flow again. */
  stalled(detail: string): void;
  resumed(): void;
  signInRequired(): void;
  /** Outcomes were lost uncounted (a dead worker, an abandoned drain, `dispose(true)`): a population can no longer
      be complete. */
  incomplete?(): void;
}
export interface ProbeChannel {
  /** Outcomes count from now on; probes sent before never enter a population. */
  measure(): void;
  /** Stops sending; probes in flight drain to their own deadlines (at most 10 s) and resolve outside the window. */
  stop(): Promise<void>;
  /** Immediate release; `incomplete` first reports the probes it cuts off. */
  dispose(incomplete?: boolean): void;
}
export type OpenProbeChannel = (options: ProbeChannelOptions, events: ProbeChannelEvents) => ProbeChannel;

/** The test settings as saved (localStorage `graphite-meter:v1` → `config`); the shape is a compatibility boundary. */
export interface TestConfig {
  stages: Record<Stage, boolean>;
  /** Transfer stages probe no loaded latency while the latency stage is off. */
  skipLoadedLatencyWhenStageOff: boolean;
  duration: { warmupMs: number; latencyMs: number; downloadMs: number; uploadMs: number; bidirectionalMs: number };
  pingCadence: PingCadence;
  loadedPingCadence: PingCadence;
  /** Automatic: HTTP/1.1 lanes per direction at most; forced: exactly this many on every protocol. */
  transferStreams: { mode: "auto" | "forced"; count: number };
  /** Lists the WebTransport datagram path among throughput choices. */
  experimentalDatagramThroughput: boolean;
  /** Saved selections: "auto", "protocol:<http1|http2|http3>", "transport:<kind>", "<origin>", "<origin>::wt[dg]". */
  transports: { throughputTarget: string; latencyTarget: string };
  /** Finish stable stages early. */
  adaptive: boolean;
  visualization: { throughputMaxBytesPerSec: number | "auto" };
}
