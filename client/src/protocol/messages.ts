// What the engine and its workers tell each other; both sides are this code, so messages are typed, not validated.
import type { ProgressRecord } from "./progress";
import type { FailureReason } from "./reasons";

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

/** How a lane, feed or channel ended. With `retry` a reconnect may recover it. With `rotate` the server no longer knows
    the upload id, so the run may replace the receiver once. */
export interface Ending {
  reason: FailureReason;
  retry: boolean;
  detail: string;
  rotate?: boolean;
  retryAfterMs?: number;
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

/** Download bytes counted since the previous report, in the measurement epoch they belong to. */
interface BytesReport {
  type: "bytes";
  bytes: number;
  epoch: number;
}

/** A worker's terminal report is an ending classified from the server's answer, or a network break, which the owner
    classifies because a sign-in or grant that ended shows first as one. */
export type EndReport = { type: "ended"; ending: Ending } | { type: "broken"; detail: string };

/** A lane answers `flush` with its epoch's download total as counted by `at`. */
export interface Flushed {
  type: "flushed";
  total: number;
  at: number;
}

/** A lane worker reopens with `download` or `upload` after each ending and stops by termination. */
export type LaneCommand =
  | { type: "download"; url: string; request: RequestRecipe; epoch: number; streams: number }
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
  | { type: "measure"; epoch: number }
  | { type: "flush"; at: number };

/** `posted` is one completed upload POST timed on the worker clock, as presentation evidence, never a measurement. */
export type LaneReport = BytesReport | { type: "posted"; bytes: number; elapsedMs: number } | EndReport | Flushed;

/** A session worker (re)dials with `download` or `upload` after its previous ending. `stop` finalizes an upload and
    is acknowledged by `stopped` whatever ended the session. `flush` is a lane's. */
export type SessionCommand =
  | { type: "download"; url: string; ticket: TicketRecipe | null; datagrams: boolean; epoch: number }
  | {
      type: "upload";
      url: string;
      ticket: TicketRecipe | null;
      datagrams: boolean;
      streams: number;
      /** The DELETE that finalizes the upload id. */
      finalize: { url: string; request: RequestRecipe };
    }
  | { type: "measure"; epoch: number }
  | { type: "flush"; at: number }
  | { type: "stop" };

/** An upload session relays its feed's observations. A refusal record ends the session instead. */
export type FeedRecord = Exclude<ProgressRecord, { type: "error" }>;
export type SessionReport =
  | BytesReport
  | { type: "record"; record: FeedRecord }
  | EndReport
  | { type: "stopped" }
  | Flushed;

export type ProbeCommand =
  | {
      type: "start";
      url: string;
      transport: "websocket" | "webtransport";
      ticket: TicketRecipe | null;
      /** Start-to-start spacing in ms. With null a probe leaves on each reply. */
      intervalMs: number | null;
      maxInFlight: number;
    }
  | { type: "measure" }
  | { type: "pace"; intervalMs: number | null; maxInFlight?: number }
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
  | { type: "drained" };
