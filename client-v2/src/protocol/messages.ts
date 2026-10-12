// What the engine's owners and its workers tell each other: a lane worker moves one HTTP fetch lane, a session worker
// one WebTransport session, a probe worker one latency bus. Both sides are this code, so messages are typed, not
// validated.
import type { Ending, ProbeOutcome, RequestRecipe, TicketRecipe } from "../engine/types";
import type { ProgressRecord } from "./progress";

/** Download bytes counted since the previous report, at most every 50 ms, in the measurement epoch they belong to. */
export interface BytesReport {
  type: "bytes";
  bytes: number;
  epoch: number;
}

/** A worker's terminal report: an ending classified from the server's answer, or a network break, which the owner
    classifies because a login or grant that ended shows first as one. */
export type EndReport = { type: "ended"; ending: Ending } | { type: "broken"; detail: string };

/** A lane worker (re)opens with `download` or `upload` after its previous ending; it stops by termination. */
export type LaneCommand =
  | { type: "download"; url: string; request: RequestRecipe; epoch: number }
  | { type: "upload"; url: string; request: RequestRecipe }
  | { type: "measure"; epoch: number };

/** `posted` is one completed upload POST, timed on the worker clock: presentation evidence, never a measurement. */
export type LaneReport = BytesReport | { type: "posted"; bytes: number; elapsedMs: number } | EndReport;

/** A session worker (re)dials with `download` or `upload` after its previous ending; `stop` finalizes an upload and
    is acknowledged by `stopped` whatever ended the session. */
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
  | { type: "stop" };

/** An upload session relays its feed's observations; a refusal record ends the session instead. */
export type FeedRecord = Exclude<ProgressRecord, { type: "error" }>;
export type SessionReport = BytesReport | { type: "record"; record: FeedRecord } | EndReport | { type: "stopped" };

export type ProbeCommand =
  | {
      type: "start";
      url: string;
      transport: "websocket" | "webtransport";
      ticket: TicketRecipe | null;
      /** Start-to-start spacing in ms; null probes on each reply. */
      intervalMs: number | null;
      maxInFlight: number;
    }
  | { type: "measure" }
  | { type: "pace"; intervalMs: number | null; maxInFlight?: number }
  /** Ends the measured window; `drained` acknowledges it once the window's probes resolved. */
  | { type: "stop" };

/** Outcomes cross with `observedAt` on epoch ms, since realm clocks differ; the owner translates them in place. */
export type ProbeReport =
  | { type: "ready" }
  | { type: "outcomes"; outcomes: ProbeOutcome[] }
  | { type: "interrupted"; count: number; reason: "unresolved" | "send-failed" }
  | { type: "stalled"; detail: string }
  | { type: "resumed" }
  | { type: "sign-in" }
  | { type: "drained" };
