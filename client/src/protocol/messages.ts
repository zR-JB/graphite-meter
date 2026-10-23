// What the engine and its workers tell each other; both sides are this code, so messages are typed, not validated.
import type { ProgressRecord } from "./progress";
import type { FailureReason } from "./reasons";

/** Everything a request needs, as plain data a worker can take. */
export interface RequestRecipe {
  headers: Record<string, string>;
  credentials: RequestCredentials;
  redirect: RequestRedirect;
}

export interface TicketRecipe {
  url: string;
  request: RequestRecipe;
}

/** How a lane, feed or channel ended; `retry` may recover it, `rotate` lets the run replace an upload id the server forgot. */
export interface Ending {
  reason: FailureReason;
  retry: boolean;
  detail: string;
  rotate?: boolean;
  retryAfterMs?: number;
  /** A server's idle or lifetime bound ended it, so a reopen is expected to work and spends none of the stage's. */
  bound?: boolean;
}

/** One resolved probe on the page's monotonic clock; a reply after the stage's cutoff stays out of the window (`inWindow: false`). */
export interface ProbeOutcome {
  rttMs: number;
  timedOut: boolean;
  handlingMs: number | null;
  observedAt: number;
  inWindow: boolean;
}

/** Download bytes counted since the previous report, in the measurement epoch they belong to. */
interface BytesReport {
  type: "bytes";
  bytes: number;
  epoch: number;
}

/** A worker's terminal report: an ending from the server's answer, or a network break the owner classifies, as an ended sign-in shows as one. */
export type EndReport = { type: "ended"; ending: Ending } | { type: "broken"; detail: string };

/** A lane answers `flush` with its epoch's download total as counted by `at`. */
export interface Flushed {
  type: "flushed";
  total: number;
  at: number;
}

/** A lane worker reopens with `download` or `upload` after each ending; `stop` lets a download's body end, which `stopped` acknowledges. */
export type LaneCommand =
  | {
      type: "download";
      url: string;
      request: RequestRecipe;
      epoch: number;
      streams: number;
      /** Epoch ms by which the server's clock ends each body (`ms=`), or null while the stage's end is not yet known. */
      until: number | null;
    }
  /** The stage's planned end moved: later bodies end by this epoch ms instead. */
  | { type: "until"; until: number }
  | {
      type: "upload";
      url: string;
      request: RequestRecipe;
      streams: number;
      /** Upload lanes posting at once across the run's servers. */
      sharing: number;
      /** The server's idle round trip, which sizes POSTs around their turnaround, or null when unknown. */
      rttMs: number | null;
    }
  | { type: "measure"; epoch: number; at: number }
  | { type: "flush"; at: number }
  | { type: "stop" };

/** `posted` is one completed upload POST timed on the worker clock, as presentation evidence, never a measurement. */
export type LaneReport =
  | BytesReport
  | { type: "posted"; bytes: number; elapsedMs: number }
  | EndReport
  | { type: "stopped" }
  | Flushed;

/** A session worker (re)dials with `download` or `upload`; `stop` finalizes an upload and is acknowledged by `stopped`. */
export type SessionCommand =
  | {
      type: "download";
      url: string;
      ticket: TicketRecipe | null;
      datagrams: boolean;
      epoch: number;
    }
  | {
      type: "upload";
      url: string;
      ticket: TicketRecipe | null;
      datagrams: boolean;
      streams: number;
      /** The DELETE that finalizes the upload id. */
      finalize: { url: string; request: RequestRecipe };
    }
  | { type: "measure"; epoch: number; at: number }
  | { type: "flush"; at: number }
  | { type: "stop" };

/** A WebTransport dial settled: it opened, or it failed, lapsed or was abandoned first, which Chrome's throttle counts. */
export interface Handshake {
  type: "handshake";
  opened: boolean;
}

/** An upload session relays its feed's observations. A refusal record ends the session instead. */
export type FeedRecord = Exclude<ProgressRecord, { type: "error" }>;
export type SessionReport =
  | BytesReport
  | { type: "record"; record: FeedRecord }
  | EndReport
  | Handshake
  | { type: "stopped" }
  | Flushed;

export type ProbeCommand =
  | {
      type: "start";
      url: string;
      transport: "websocket" | "webtransport";
      ticket: TicketRecipe | null;
      intervalMs: number | null;
      maxInFlight: number;
    }
  | { type: "measure" }
  | { type: "pace"; intervalMs: number | null; maxInFlight?: number }
  /** Dials again after the bus stalled; the worker never redials on its own. */
  | { type: "dial" }
  /** Ends the measured window. `drained` acknowledges it once the window's probes resolved. */
  | { type: "stop" };

/** Outcomes cross with `observedAt` on epoch ms, since realm clocks differ. The owner translates them in place. */
export type ProbeReport =
  | { type: "ready" }
  | { type: "outcomes"; outcomes: ProbeOutcome[] }
  | { type: "interrupted"; count: number; reason: "unresolved" | "send-failed" }
  | { type: "stalled"; detail: string }
  | { type: "resumed" }
  | { type: "sign-in" }
  | Handshake
  | { type: "drained" };
