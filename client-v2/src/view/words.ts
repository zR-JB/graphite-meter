// The words every view shares, one per concept: stages and their state in the shown run, outcomes, run phases,
// failure reasons, latency populations, paths, transports, HTTP versions, readiness, connectivity and servers, with
// tones.
import type { Readiness } from "../engine/net/connection";
import type { Outcome, StageStatus } from "../engine/run/result";
import type { Phase } from "../engine/run/run";
import { planned, STAGES, type Stage } from "../engine/run/schedule";
import type { HttpProtocol, LatencyTarget, Role, TestConfig, ThroughputTarget } from "../engine/types";
import { FAILURE_REASONS, type FailureReason } from "../protocol/reasons";
import { fmtDuration } from "./format";

/** The hues `[data-tone]` maps (styles/tokens.css): each stage's own, the warmup's, and the status tones. */
export type Tone = Stage | "warmup" | "ok" | "warn" | "err" | "neutral";
/** A status word; a null tone leaves its subject's own hue. */
export interface Word {
  label: string;
  tone: Tone | null;
}

/** `short` names a stage where space is tight: chips, lane rows, History column heads. */
export const STAGE = {
  latency: { label: "Latency", short: "Latency", icon: "ping", tone: "latency" },
  download: { label: "Download", short: "Download", icon: "download", tone: "download" },
  upload: { label: "Upload", short: "Upload", icon: "upload", tone: "upload" },
  bidirectional: { label: "Bidirectional", short: "Bi-dir", icon: "bidirectional", tone: "bidirectional" },
} as const satisfies Record<Stage, { label: string; short: string; icon: string; tone: Tone }>;

/** What the run is doing, as the status bar and the dial say it; a finished run reads its outcome. */
export const PHASE = {
  idle: "Not started",
  authenticating: "Checking sign-in",
  connecting: "Checking paths",
  blocked: "Test cannot start",
  "start-failed": "Test could not start",
  warmup: "Warmup",
  latency: STAGE.latency.label,
  download: STAGE.download.label,
  upload: STAGE.upload.label,
  bidirectional: STAGE.bidirectional.label,
  stopped: "Stopped",
  error: "Failed",
} as const satisfies Record<Phase, string> & Record<string, string>;

export const OUTCOME = {
  complete: { label: "Complete", tone: null },
  partial: { label: "Partial", tone: "warn" },
  incomplete: { label: "Incomplete", tone: "warn" },
} as const satisfies Record<Outcome, Word>;

/** A stage in the shown run. `skipped`: the run left it out; `unreached`: the run ended without reaching or settling
    it. */
export type StageState =
  | "skipped"
  | "upcoming"
  | "warmup"
  | "running"
  | "recovering"
  | "complete"
  | "partial"
  | "failed"
  | "stopped"
  | "unreached";

/** One word and tone per stage state on cards, chips and History alike. */
export const STAGE_STATE = {
  skipped: { label: "Skipped", tone: "neutral" },
  upcoming: { label: "Upcoming", tone: null },
  warmup: { label: PHASE.warmup, tone: null },
  running: { label: "Running", tone: null },
  recovering: { label: "Recovering", tone: "warn" },
  complete: { label: "Complete", tone: null },
  partial: { label: "Partial", tone: "warn" },
  failed: { label: "Failed", tone: "err" },
  stopped: { label: PHASE.stopped, tone: "neutral" },
  unreached: { label: "Not reached", tone: "neutral" },
} as const satisfies Record<StageState, Word>;

/** A stage the next run includes but the shown run did not run, as its chip names it. */
export const NEXT_RUN = { label: "Next run", tone: null } as const satisfies Word;

/** What the runner has said about one stage of the shown run. */
export interface StageFacts {
  /** The run includes the stage. */
  planned: boolean;
  /** Its status once it ended. */
  settled: StageStatus | null;
  /** The run's phase while this is its stage, kept once the run ends; null until the run moves to it. */
  phase: Phase | null;
  stalled: boolean;
  /** How the run ended; null while it runs. */
  ended: "complete" | "stopped" | "error" | null;
}

/** The plan decides first, then a settled status; a stall reads Recovering, in warmup too. A stopped or failed run
    leaves the stage it was in stopped or failed, and any other stage it never settled unreached. Checking paths
    before the first stage is not that stage yet. */
export function stageState({ planned, settled, phase, stalled, ended }: StageFacts): StageState {
  if (!planned || settled === "not-run") return "skipped";
  if (settled) return settled;
  const began = phase !== null && phase !== "connecting";
  if (ended) return !began || ended === "complete" ? "unreached" : ended === "stopped" ? "stopped" : "failed";
  if (!began) return "upcoming";
  return stalled ? "recovering" : phase === "warmup" ? "warmup" : "running";
}

/** A reason this build does not know, as a newer build may save, reads as a measurement issue. */
export const reasonLabel = (reason: string): string =>
  Object.hasOwn(FAILURE_REASONS, reason) ? FAILURE_REASONS[reason as FailureReason] : "Measurement issue";

/** Idle latency is probed in the latency stage, loaded latency during the transfers. */
export const LATENCY = {
  idle: { label: "Idle latency", short: "Idle" },
  loaded: { label: "Loaded latency", short: "Loaded" },
} as const;

/** Each stage's latency population, as the lanes and issue lines name it. */
export const POPULATION = {
  latency: LATENCY.idle.short,
  download: "Loaded down",
  upload: "Loaded up",
  bidirectional: "Loaded bi-dir",
} as const satisfies Record<Stage, string>;

/** Each role's connection to a server, as Settings, Details and History name it. */
export const PATH = { throughput: "Throughput path", latency: "Latency path" } as const satisfies Record<Role, string>;

type Transport = (ThroughputTarget | LatencyTarget)["transport"];
const TRANSPORT: Record<Transport, string> = {
  "fetch-stream": "Fetch streams",
  websocket: "WebSocket",
  webtransport: "WebTransport streams",
  "webtransport-datagram": "WebTransport datagrams",
};

/** A transport in its role, or null for one this build does not know. Bare WebTransport names the session, whose
    streams carry throughput and whose datagrams carry latency. */
export function transportLabel(transport: string, role: Role): string | null {
  const kind = transport === "webtransport" && role === "latency" ? "webtransport-datagram" : transport;
  return Object.hasOwn(TRANSPORT, kind) ? TRANSPORT[kind as Transport] : null;
}

export const HTTP_VERSION = {
  http1: "HTTP/1.1",
  http2: "HTTP/2",
  http3: "HTTP/3",
  negotiated: "Negotiated HTTP",
} as const satisfies Record<HttpProtocol, string>;

/** A server's or path's readiness, and what a blocker or a run holding the path makes of it. */
export const READINESS = {
  unchecked: { label: "Recheck needed", tone: "warn" },
  checking: { label: "Checking", tone: "neutral" },
  verified: { label: "Ready", tone: "ok" },
  "sign-in": { label: "Sign in", tone: "warn" },
  failed: { label: "Failed", tone: "err" },
  blocked: { label: PHASE.blocked, tone: "warn" },
  "in-use": { label: "In use", tone: "ok" },
  used: { label: "Used", tone: "neutral" },
} as const satisfies Record<Readiness | "blocked" | "in-use" | "used", Word>;

/** What each readiness means, under its word as the title line. */
export const READINESS_TIP = {
  unchecked: `${READINESS.unchecked.label}\nThe last check expired or a path changed\nIt reruns before the test`,
  checking: `${READINESS.checking.label}\nAsking each selected server which paths it offers`,
  verified:
    `${READINESS.verified.label}\nThe selected servers and paths passed their check\n` +
    "Recent checks are reused; expired ones rerun before a test",
  "sign-in": `${READINESS["sign-in"].label}\nA selected server only admits signed-in clients`,
  failed: `${READINESS.failed.label}\nA selected server or path did not answer its check`,
} satisfies Record<Readiness, string>;

/** The connection as the idle probes judge it between runs; a stalled run is Recovering wherever it shows. */
export const CONNECTIVITY = {
  connected: { label: "Connected", tone: "ok" },
  degraded: { label: "Degraded", tone: "warn" },
  unstable: { label: "Unstable", tone: "err" },
  offline: { label: "Offline", tone: "err" },
  checking: READINESS.checking,
  recovering: STAGE_STATE.recovering,
} as const satisfies Record<string, Word>;
export type Connectivity = keyof typeof CONNECTIVITY;

/** A server by name and location, a city-named one read once: "Nuremberg" in "Nuremberg, DE" reads "Nuremberg, DE". */
export function serverLabel({ name, location = "" }: { name: string; location?: string }): string {
  const lowerName = name.toLowerCase();
  const lowerLocation = location.toLowerCase();
  if (!lowerLocation || lowerName.includes(lowerLocation)) return name;
  return lowerLocation.startsWith(lowerName) ? location : `${name}, ${location}`;
}

/** The all-servers choice counts who stayed to the end, or, when none did, who measured anything. */
export function allServersLabel(total: number, stayed: number, measured: number): string {
  const shown = stayed || measured;
  return shown < total ? `${shown} of ${total} servers` : `All ${total} servers`;
}

const LIST = new Intl.ListFormat("en-GB", { type: "conjunction" });

/** Why Start is refused when planned stages outlast the shortest stage limit of the selected servers; "" when none
    does. */
export function stageLimitError(config: TestConfig, limit: { ms: number; server: string | null }): string {
  const over = STAGES.filter((stage) => planned(config, stage) && config.duration[`${stage}Ms`] > limit.ms);
  if (!limit.server || !over.length) return "";
  const stages = LIST.format(over.map((stage) => STAGE[stage].label));
  return `${limit.server} allows stages up to ${fmtDuration(limit.ms, 0)}; shorten the ${stages} stage${over.length > 1 ? "s" : ""}.`;
}
