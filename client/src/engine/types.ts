// Defines the engine's shared vocabulary, from servers and their verified paths, credentials and evidence to the
// contract between the run and the resources each server opens for a stage.
import type { Probe } from "../protocol/documents";
import type { Ending, ProbeOutcome, RequestRecipe, TicketRecipe } from "../protocol/messages";
import type { FailureReason } from "../protocol/reasons";
import type { Activity, Stage } from "./run/schedule";

export type { Ending, FailureReason, Probe, ProbeOutcome, RequestRecipe, Stage, TicketRecipe };
export type Direction = "down" | "up";
export const ROLES = ["throughput", "latency"] as const;
export type Role = (typeof ROLES)[number];
export type PingCadence = "reply-driven" | "fast" | "medium" | "slow";

/** Fixed start-to-start probe spacing. Reply-driven has none. */
export const CADENCE_MS: Record<PingCadence, number | null> = {
  "reply-driven": null,
  fast: 80,
  medium: 250,
  slow: 600,
};

/** A catalog server as results and history name it. */
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

/** A receiver observation holds an upload id's bytes and receiver nanoseconds, and when the client saw it. */
export interface Checkpoint {
  id: string;
  bytes: number;
  nanos: number;
  requestedAtMs?: number;
  receivedAtMs: number;
}

export type HttpProtocol = "http1" | "http2" | "http3" | "negotiated";

/** An advertised way to reach a server. `id` is what a saved selection names (the origin, `::wt` or `::wtdg`). */
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

/** Holds what one origin of a server offers this page, or why the browser cannot use it. */
export interface OriginTargets<T> {
  blocked: string | null;
  targets: T[];
}

/** A server's preflight as this page can use it. */
export interface Discovery {
  generation: string;
  engineVersion: string;
  server: { name: string; location?: string };
  /** The longest stage the server admits. */
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

/** Records what a working path was proven for. Automatic stays valid while its first candidate does not change, an
    explicit choice while it resolves to the verified target. */
export interface VerifiedFor {
  selection: string;
  head: string;
}
export interface ThroughputPath {
  /** Carries the bytes, as a fetch target with the protocol the browser negotiated or a WebTransport target. */
  target: ThroughputTarget;
  /** The HTTP view of the target's origin serves probes, upload sessions, checkpoints and the HTTP progress feed. */
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
  /** The median of the check's replies, or null without one. */
  rttMs: number | null;
  generation: string;
  verifiedAt: number;
  verifiedFor: VerifiedFor;
}

/** A server ready to run is immutable input to one run. */
export interface PreparedServer {
  server: ServerIdentity;
  auth: ServerAuth;
  discovery: Discovery;
  throughput: ThroughputPath;
  /** Null when no planned stage probes latency. */
  latency: LatencyPath | null;
}

/** A server's credentials are none, the page's own sign-in, or a measurement grant. Requests never leave the server's
    allowed destinations, and a grant never appears in a URL. */
export interface ServerAuth {
  readonly kind: "public" | "session" | "grant";
  /** Ms until the grant ends by whichever clock says less, or Infinity for credentials without an end of their own. */
  left(): number;
  /** Throws SignInRequired when the grant is missing or ended. */
  request(url: string, method: string): RequestRecipe;
  /** Sends a control request at high priority with the recipe applied. A 403 carrying the sign-in marker is thrown as
      SignInRequired with its body released. */
  fetch(url: string, init?: RequestInit): Promise<Response>;
  /** Null when a socket needs no ticket, on public servers and for a WebSocket riding the page's own cookie. */
  ticket(origin: string, route: string): TicketRecipe | null;
  /** Asks whether a broken stream was the sign-in or grant ending, by the one cheap check per kind, at most 3 s. */
  ended(signal?: AbortSignal): Promise<boolean>;
}

export class SignInRequired extends Error {
  constructor(readonly server: ServerIdentity) {
    super(`Sign in to ${server.name}`);
  }
}

/** A non-terminal stall means a direction went quiet or its lanes are reconnecting. */
export interface Stall {
  reason: FailureReason;
  detail: string;
  direction?: Direction;
  rotate?: boolean;
}

/** One server's stage resources report these to the run. */
export interface StageEvents {
  /** Counts client-consumed payload bytes of a measured download. */
  download(bytes: number): void;
  /** Reports a receiver observation of a measured upload. */
  receiver(checkpoint: Checkpoint): void;
  /** Counts client-consumed bytes before the measured window, which only the live rate reads. */
  warmupDownload(bytes: number): void;
  /** Reports a receiver observation before the measured window, which only the live rate reads. */
  warmupReceiver(checkpoint: Checkpoint): void;
  probes(outcomes: readonly ProbeOutcome[]): void;
  /** Counts probes that will get no verdict, lost with their connection or never sent. Neither is a timeout. */
  probesInterrupted(count: number, reason: "unresolved" | "send-failed"): void;
  /** The probe population's accounting can no longer be complete. */
  probesIncomplete(): void;
  stalled(stall: Stall): void;
  resumed(): void;
  latencyStalled(detail: string): void;
  latencyResumed(): void;
  failed(reason: FailureReason, message: string): void;
  signInRequired(role: Role): void;
  /** Reports a locally timed upload POST, which is presentation evidence while a receiver pauses. */
  uploadHint(lane: number, bytes: number, elapsedMs: number): void;
}

/** A server's lanes for one stage, as the browser's connection budget dealt them. */
export type StageLanes = Pick<StageSetup, "streams" | "gating">;

export interface StageSetup {
  server: PreparedServer;
  activity: Activity;
  /** Lanes per direction for this server and stage, 0 for a direction the stage does not move. */
  streams: Record<Direction, number>;
  /** Upload lanes across every server of the stage. */
  uploadLanes: number;
  /** Readiness waits for these lanes, those an HTTP/1.1 origin's pool runs at once, or all of `streams` if absent. */
  gating?: Record<Direction, number>;
  /** The latency stage probes at the idle cadence, any other stage at the loaded one. */
  cadence: PingCadence;
  /** Distinguishes this run's requests from cached ones. */
  seed: string;
  events: StageEvents;
  /** Run ms, the clock checkpoints are stamped with. */
  now(): number;
  /** The first stage that probes opens the server's latency bus for the run, and each later one adopts it. */
  bus: { current: LatencyBus | null };
}

/** One server's resources for one stage, shared by its warmup and measured window. */
export interface StageResources {
  /** Opens the carriers, receiver and probe channel, and resolves once they work, within 3.5 s. Loaded latency never
      holds throughput back. */
  prepare(signal: AbortSignal): Promise<void>;
  /** The measured window starts, so evidence from now on counts. */
  measure(): void;
  /** Asks for a receiver checkpoint. `final` issues fresh requests and retries a miss within 1.5 s. */
  checkpoint(signal: AbortSignal, final?: boolean): Promise<Checkpoint | null>;
  /** Resolves once the download bytes the lanes had counted by `cutoff` (page time) reached `download`, with the
      latest page time those counts stand at (null without download lanes). It never rejects. */
  flush(cutoff: number): Promise<number | null>;
  /** Replaces an upload id the server no longer knows, as the run's one rotation per server. */
  replaceUpload(signal: AbortSignal): Promise<void>;
  /** Ends gracefully. Probes drain to their deadlines, sessions finalize, then everything is released. */
  finish(): Promise<void>;
  /** Releases at once. `incomplete` first reports the probes it cuts off. */
  discard(incomplete?: boolean): void;
}

export type OpenStage = (setup: StageSetup) => StageResources;

export class ServerBusyError extends Error {}

/** A failure that ended a stage's preparation, with the reason it would have reported as an event. */
export class StageFailure extends Error {
  constructor(
    readonly reason: FailureReason,
    message: string,
  ) {
    super(message);
  }
}

/** Opens a latency bus to one server, a WebSocket or WebTransport datagrams, driven from a worker. */
export interface ProbeChannelOptions {
  target: LatencyTarget;
  auth: ServerAuth;
  /** Fixed start-to-start spacing in ms, or null for reply-driven probing. */
  intervalMs: number | null;
  /** Bounds the probes in flight. A full window skips a send, never times one out. */
  maxInFlight: number;
}
export interface ProbeChannelEvents {
  /** The first matched reply on each connection. */
  ready(): void;
  outcomes(batch: readonly ProbeOutcome[]): void;
  interrupted(count: number, reason: "unresolved" | "send-failed"): void;
  /** The connection was lost and is reconnecting (once per outage). `resumed` follows when replies flow again. */
  stalled(detail: string): void;
  resumed(): void;
  signInRequired(): void;
  /** Outcomes were lost uncounted, so a population can no longer be complete. */
  incomplete?(): void;
}
export interface ProbeChannel {
  /** Outcomes count from now on. Probes sent before never enter a population. */
  measure(): void;
  /** Probes at this start-to-start spacing (null for on each reply), with at most `maxInFlight` in flight when given,
      from the next send on. Probes in flight keep their deadlines. */
  pace(intervalMs: number | null, maxInFlight?: number): void;
  /** Ends the measured window. Its probes in flight drain to their own deadlines and resolve outside it. The bus
      probes on, unmeasured, until disposed. */
  stop(): Promise<void>;
  /** Releases at once. `incomplete` first reports the probes it cuts off. */
  dispose(incomplete?: boolean): void;
}
export type OpenProbeChannel = (options: ProbeChannelOptions, events: ProbeChannelEvents) => ProbeChannel;
/** A probe channel that outlives its stage. */
export interface LatencyBus extends ProbeChannel {
  /** Its events go to `events` from now on, told at once whether the bus answers or stalled. False once it ended. */
  adopt(events: ProbeChannelEvents): boolean;
}

/** The test settings are saved in localStorage `graphite-meter:v1` → `config`. */
export interface TestConfig {
  stages: Record<Stage, boolean>;
  /** Transfer stages probe no loaded latency while the latency stage is off. */
  skipLoadedLatencyWhenStageOff: boolean;
  duration: { warmupMs: number; latencyMs: number; downloadMs: number; uploadMs: number; bidirectionalMs: number };
  pingCadence: PingCadence;
  loadedPingCadence: PingCadence;
  /** Automatic opens at most this many HTTP/1.1 lanes per direction, forced exactly this many on every protocol. */
  transferStreams: { mode: "auto" | "forced"; count: number };
  /** Lists the WebTransport datagram path among throughput choices. */
  experimentalDatagramThroughput: boolean;
  /** A saved selection is "auto", "protocol:<http1|http2|http3>", "transport:<kind>", "<origin>" or "<origin>::wt[dg]". */
  transports: { throughputTarget: string; latencyTarget: string };
  /** Finishes stable stages early. */
  adaptive: boolean;
  visualization: { throughputMaxBytesPerSec: number | "auto" };
}
