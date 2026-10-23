// The engine's shared vocabulary and the contract between the run and each server's stage resources.
import type { Probe } from "../protocol/documents";
import type { Ending, ProbeOutcome, RequestRecipe, TicketRecipe } from "../protocol/messages";
import type { FailureReason } from "../protocol/reasons";
import type { Activity, Stage } from "./run/schedule";

export type { Ending, FailureReason, Probe, ProbeOutcome, RequestRecipe, Stage, TicketRecipe };
export type Direction = "down" | "up";
export const ROLES = ["throughput", "latency"] as const;
export const MAX_SELECTED = 4;
export const SELECTION_REFUSED = `Select 1 to ${MAX_SELECTED} test servers`;
export type Role = (typeof ROLES)[number];
export type PingCadence = "reply-driven" | "fast" | "medium" | "slow";

/** Fixed start-to-start probe spacing; reply-driven has none. */
export const CADENCE_MS: Record<PingCadence, number | null> = {
  "reply-driven": null,
  fast: 80,
  medium: 250,
  slow: 600,
};

export interface ServerIdentity {
  id: string;
  url: string;
  name: string;
  location?: string;
}

export const identityOf = ({ id, url, name, location }: ServerIdentity): ServerIdentity => ({
  id,
  url,
  name,
  ...(location ? { location } : {}),
});

/** A receiver observation: an upload id's bytes and receiver nanoseconds, and when the client saw it. */
export interface Checkpoint {
  id: string;
  bytes: number;
  nanos: number;
  requestedAtMs?: number;
  receivedAtMs: number;
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

export interface Discovery {
  generation: string;
  engineVersion: string;
  server: { name: string; location?: string };
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

/** A path proven to work, and the selection it was proven for. */
export interface VerifiedFor {
  selection: string;
  head: string;
  /** Why Automatic verified a later candidate than the head. */
  fallback?: string;
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
  latency: LatencyPath | null;
}

/** A server's credentials: none, the page's sign-in, or a measurement grant, which never appears in a URL. */
export interface ServerAuth {
  readonly kind: "public" | "session" | "grant";
  /** Ms until the grant ends by whichever clock says less; Infinity for credentials without an end of their own. */
  left(): number;
  /** Throws SignInRequired when the grant is missing or ended. */
  request(url: string, method: string): RequestRecipe;
  /** A control request at high priority; a 403 with the sign-in marker throws SignInRequired, its body released. */
  fetch(url: string, init?: RequestInit): Promise<Response>;
  /** Null when a socket needs no ticket: public servers, and a WebSocket riding the page's own cookie. */
  ticket(origin: string, route: string): TicketRecipe | null;
  /** Whether a broken stream was the sign-in or grant ending: the one cheap check per kind, at most 3 s. */
  ended(signal?: AbortSignal): Promise<boolean>;
}

export class SignInRequired extends Error {
  constructor(readonly server: ServerIdentity) {
    super(`Sign in to ${server.name}`);
  }
}

/** A non-terminal stall: a direction went quiet or its lanes are reconnecting. */
export interface Stall {
  reason: FailureReason;
  detail: string;
  direction?: Direction;
  rotate?: boolean;
}

export interface StageEvents {
  download(bytes: number): void;
  receiver(checkpoint: Checkpoint): void;
  /** Client-consumed bytes before the measured window, which only the live rate reads. */
  warmupDownload(bytes: number): void;
  /** A receiver observation before the measured window, which only the live rate reads. */
  warmupReceiver(checkpoint: Checkpoint): void;
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
  uploadLanes: number;
  /** The lanes readiness waits for: those an HTTP/1.1 origin's pool runs at once; all of `streams` when absent. */
  gating?: Record<Direction, number>;
  /** The probe spacing of this stage: the idle cadence for the latency stage, the loaded one otherwise. */
  cadence: PingCadence;
  /** Distinguishes this run's requests from cached ones. */
  seed: string;
  events: StageEvents;
  /** Run ms, the clock checkpoints are stamped with. */
  now(): number;
  /** The server's latency bus for the run: opened by the first stage that probes, adopted by each later one. */
  bus: { current: LatencyBus | null };
}

/** One server's resources for one stage, shared by its warmup and measured window. */
export interface StageResources {
  /** Opens the carriers, receiver and probe channel, resolving once they work, within 3.5 s; loaded latency never holds throughput back. */
  prepare(signal: AbortSignal): Promise<void>;
  /** Opens the measured window at page time `at`. */
  measure(at: number): void;
  /** A receiver checkpoint; `final` issues fresh requests and retries a miss within 1.5 s. */
  checkpoint(signal: AbortSignal, final?: boolean): Promise<Checkpoint | null>;
  /** The window's download bytes the lanes counted by page time `at`, or null without every answer; never rejects. */
  countAt(at: number): Promise<number | null>;
  /** Resolves once the download bytes counted by `cutoff` reached `download`, with the latest time they stand at; never rejects. */
  flush(cutoff: number): Promise<number | null>;
  /** Replaces an upload id the server no longer knows: the run's one rotation per server. */
  replaceUpload(signal: AbortSignal): Promise<void>;
  /** Graceful end: probes drain to their deadlines, sessions finalize, then everything is released. */
  finish(): Promise<void>;
  /** Immediate release; `incomplete` first reports the probes it cuts off. */
  discard(incomplete?: boolean): void;
}

export type OpenStage = (setup: StageSetup) => StageResources;

export class ServerBusyError extends Error {}

export function* causes(error: unknown): Generator<Error> {
  for (let depth = 0; error instanceof Error && depth < 16; error = error.cause, depth++)
    yield error;
}

const NETWORK_FAILURE =
  /failed to fetch|fetch failed|network(?:error| request failed)|load failed|connection (?:refused|reset|lost)/i;
/** Engines word one network failure differently. */
export const isNetworkFailure = (error: unknown) =>
  [...causes(error)].some(
    ({ name, message }) => name === "NetworkError" || NETWORK_FAILURE.test(message),
  );

/** A failure that ended a stage's preparation, with the reason it would have reported as an event. */
export class StageFailure extends Error {
  constructor(
    readonly reason: FailureReason,
    message: string,
  ) {
    super(message);
  }
}

/** A latency bus to one server: a WebSocket, or WebTransport datagrams, driven from a worker. */
export interface ProbeChannelOptions {
  target: LatencyTarget;
  auth: ServerAuth;
  intervalMs: number | null;
  /** Probes in flight at most; a full window skips a send, never times one out. */
  maxInFlight: number;
}
export interface ProbeChannelEvents {
  ready(): void;
  outcomes(batch: readonly ProbeOutcome[]): void;
  interrupted(count: number, reason: "unresolved" | "send-failed"): void;
  /** The connection was lost, once per outage; its owner may redial it once; `resumed` when replies flow again. */
  stalled(detail: string): void;
  resumed(): void;
  signInRequired(): void;
  /** Outcomes were lost uncounted: a population can no longer be complete. */
  incomplete?(): void;
}
export interface ProbeChannel {
  /** Outcomes count from now on; probes sent before never enter a population. */
  measure(): void;
  /** Probes at this start-to-start spacing (null: on each reply), at most `maxInFlight` in flight, from the next send on. */
  pace(intervalMs: number | null, maxInFlight?: number): void;
  /** Ends the measured window: its probes drain to their deadlines outside it, and the bus probes on, unmeasured, until disposed. */
  stop(): Promise<void>;
  /** Immediate release; `incomplete` first reports the probes it cuts off. */
  dispose(incomplete?: boolean): void;
}
export type OpenProbeChannel = (
  options: ProbeChannelOptions,
  events: ProbeChannelEvents,
) => ProbeChannel;
/** A probe channel that outlives its stage. */
export interface LatencyBus extends ProbeChannel {
  /** Its events go to `events` from now on, told at once whether the bus answers or stalled; false once it ended. */
  adopt(events: ProbeChannelEvents): boolean;
}

/** The test settings as saved (localStorage `graphite-meter:v1` → `config`). */
export interface TestConfig {
  stages: Record<Stage, boolean>;
  skipLoadedLatencyWhenStageOff: boolean;
  duration: {
    warmupMs: number;
    latencyMs: number;
    downloadMs: number;
    uploadMs: number;
    bidirectionalMs: number;
  };
  pingCadence: PingCadence;
  loadedPingCadence: PingCadence;
  /** Automatic: HTTP/1.1 lanes per direction at most; forced: exactly this many on every protocol. */
  transferStreams: { mode: "auto" | "forced"; count: number };
  experimentalDatagramThroughput: boolean;
  /** Saved selections: "auto", "protocol:<http1|http2|http3>", "transport:<kind>", "<origin>", "<origin>::wt[dg]". */
  transports: { throughputTarget: string; latencyTarget: string };
  adaptive: boolean;
  visualization: { throughputMaxBytesPerSec: number | "auto" };
}
