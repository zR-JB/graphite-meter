// The words the views share, one per concept: stages and their state in the shown run, outcomes, run phases and the
// run key, failure reasons, latency populations, paths, transports, HTTP versions, readiness, connectivity, servers,
// themes, starting over and the explainers, with tones.
import type { Theme } from "../app/preferences";
import type { Preparation, ShownPhase } from "../app/run";
import type { Readiness } from "../engine/net/connection";
import { PROTOCOL_LABEL, protocolFromNextHop } from "../engine/net/paths";
import type { Outcome, StageStatus } from "../engine/run/result";
import type { Phase } from "../engine/run/run";
import { planned, STAGES, type Stage } from "../engine/run/schedule";
import {
  CADENCE_MS,
  type HttpProtocol,
  type LatencyTarget,
  type PingCadence,
  type Role,
  type TestConfig,
  type ThroughputTarget,
} from "../engine/types";
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

/** What the run key does now; the Space hint holds every one. */
export const RUN_ACTION = { start: "Start test", stop: "Stop test", again: "Run again", cancel: "Cancel" } as const;

export function runAction(preparing: boolean, running: boolean, phase: ShownPhase): string {
  if (preparing) return RUN_ACTION.cancel;
  if (running) return RUN_ACTION.stop;
  return phase === "complete" || phase === "aborted" || phase === "error" ? RUN_ACTION.again : RUN_ACTION.start;
}

/** The run in one word, as the status strip says it: a refused start, a check under way, else the phase or the
    finished run's outcome. */
export function statusLabel(preparation: Preparation, phase: ShownPhase, outcome: Outcome | undefined): string {
  if (preparation === "blocked") return PHASE.blocked;
  if (preparation === "failed") return PHASE["start-failed"];
  if (preparation === "authenticating") return PHASE.authenticating;
  if (preparation === "checking" || preparation === "launching") return PHASE.connecting;
  if (phase === "complete") return OUTCOME[outcome ?? "complete"].label;
  if (phase === "aborted") return PHASE.stopped;
  return PHASE[phase];
}

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

export const PATH_TIP: Record<Role, string> = {
  throughput: `${PATH.throughput}\nTransport and HTTP version of the test bytes`,
  latency: `${PATH.latency}\nTransport of the probes, on its own connection`,
};

/** A path choice's second line: what it does, in a few words. */
export const PATH_NOTE: Record<Role, Record<string, string>> = {
  throughput: {
    auto: "HTTP/1.1, else HTTP/2, HTTP/3 or WebTransport",
    "protocol:http1": "Parallel connections, one stream each",
    "protocol:http2": "One connection, several streams",
    "protocol:http3": "One QUIC connection, several streams",
    "transport:webtransport": "Streams in one HTTP/3 session",
    "transport:webtransport-datagram": "Experimental unreliable datagrams",
  },
  latency: {
    auto: "WebTransport datagrams, else WebSocket",
    "transport:websocket": "Reliable messages over one connection",
    "transport:webtransport": "Unreliable messages in one HTTP/3 session",
  },
};

type Transport = (ThroughputTarget | LatencyTarget)["transport"];
export const TRANSPORT: Record<Transport, string> = {
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

/** An HTTP version as recorded or as a browser's next hop names it ("h2"); one this build does not know reads as
    given. */
export function httpVersionLabel(name: string): string {
  const known = protocolFromNextHop(name) ?? name;
  return Object.hasOwn(PROTOCOL_LABEL, known) ? PROTOCOL_LABEL[known as HttpProtocol] : name;
}

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

export const THEME = {
  light: { label: "Light", icon: "sun" },
  dark: { label: "Dark", icon: "moon" },
  auto: { label: "Auto", icon: "contrast" },
} as const satisfies Record<Theme, { label: string; icon: string }>;

/** Starting over asks first while a test runs or its result is not saved. */
export const START_OVER = {
  running: {
    title: "Stop the running test?",
    description: "The measurement in progress stops and its results are cleared.",
    cancel: "Keep running",
    confirm: "Stop test",
  },
  unsaved: {
    title: "Clear this result?",
    description: "It is not saved; starting over removes it.",
    cancel: "Keep result",
    confirm: "Clear result",
  },
} as const;

/** A cadence's name, and its word on a segment. */
export const CADENCE: Record<PingCadence, { label: string; short: string }> = {
  "reply-driven": { label: "Reply-driven", short: "Reply" },
  fast: { label: `Fast (${CADENCE_MS.fast} ms)`, short: "Fast" },
  medium: { label: `Medium (${CADENCE_MS.medium} ms)`, short: "Medium" },
  slow: { label: `Slow (${CADENCE_MS.slow} ms)`, short: "Slow" },
};
const SPACINGS = `Fast ${CADENCE_MS.fast} ms · Medium ${CADENCE_MS.medium} ms · Slow ${CADENCE_MS.slow} ms`;

/** The explanations beside words and figures: a title line, then a few short lines. */
export const TIP = {
  latency: "Idle latency\nMedian round trip on an idle connection\nThe base for added latency",
  transferred: "Transferred\nPayload bytes measured in this stage",
  peak: "Peak\nHighest mean over windows of 0.5 s or more\nNever below the headline",
  rateStability: "Stability\n100% minus the spread of the 250 ms rates\nSpread: standard deviation over the mean",
  latencyStability: "Stability\n100% minus jitter as a share of the median",
  latencyRange: "Range\nFastest to slowest reply",
  replies: "Replies\nIdle probes answered in this stage",
  timeouts: "Timeouts\nIdle probes with no reply before their deadline\nA timeout is a missing reply, not packet loss",
  jitter: "Jitter\nMean change between consecutive replies\nTimeouts are left out",
  noData: "No data\nNo bytes arrived; it counts in the average",
  noReplies: "No replies\nThese probes timed out; not packet loss",
  addedLatency: "Added latency\nLoaded minus idle median, same server",
  latencyMedian: "Median\nHalf the replies were faster, half slower",
  warmup: "Warmup\nOpens connections before each stage\n10 round trips to 4 s; never counted",
  testServers: "Test servers\nUp to 4; their speeds add up\nAll are probed; the lanes show one at a time",
  stageTime: "Duration\nEach stage's planned length\nUp to the servers' limit, 5 min by default",
  bidirectionalStage: "Bidirectional stage\nDownload and upload at once, after the others",
  earlyFinish: "Early finish\nEnds a steady stage after about half its time\nNever after a stall, gap or lost server",
  units:
    "Units\nBits: Mbit/s, as internet plans\nBytes: MB/s, 8× lower, as downloads\n" +
    "Decimal: steps of 1,000 (k, M, G)\nBinary: steps of 1,024 (Ki, Mi, Gi)",
  explanations:
    "Explanations\nOn hover: as the pointer rests on a word or figure\nOn click: only when it is clicked or tapped",
  wireRate: "Wire rate\nPayload plus the protocol headers carried with it\nAn estimate; hover a result for its parts",
  keyShortcuts:
    "Keyboard shortcuts\nS, D, H, R and T act when no field has focus\nTurn off if speech input triggers them",
  gaugeAuto: "Automatic scale\nThe chart follows the peak\nThe gauge steps 1, 2, 5, 10 of its unit",
  gaugeMax: "Maximum\nFixes the chart ceiling\nThe gauge rounds up to a 1, 2, 5 step",
  saveResults: "Save results\nKeeps the newest 2,000 runs in this browser\nNothing is uploaded",
  idleCadence: `Idle latency cadence\nReply: the next probe leaves on each reply\n${SPACINGS}`,
  loadedCadence: `Loaded latency cadence\nProbes during transfers, start to start\n${SPACINGS}`,
  skipLoadedLatency: "Skip loaded latency\nWith the Latency stage off, transfers send no probes",
  forcedStreams:
    "Streams\nParallel connections per server and direction\nForced opens the exact count, past browser limits",
  forcedStreamCount:
    "Streams per server and direction\nOpens exactly this many\nWebTransport carries at most 16 per session",
  autoStreamCount:
    "HTTP/1.1 stream limit\nParallel HTTP/1.1 requests per direction\nHTTP/2 and HTTP/3 choose their own",
  datagramThroughput:
    "Datagram throughput\nAdds WebTransport datagrams as a path\nNever resent; expect lower rates than streams",
  pathEvidence: "Evidence\nThe HTTP version each side observed",
  uploadFeed: "Upload feed\nHow the server reports upload bytes back",
  clientAddress: "Your address\nAs the server saw it: proxy header or socket\nHidden until you press the eye beside it",
  serverLoad: "Load\nTests on the server when the path was checked\nPast half its slots, they share the bandwidth",
  client: "Client\nThis page's build of browser client v2",
  serverInstance: "Server\nEngine version; the instance changes on restart",
  pretestLatency: "Pre-test latency\nMedian round trip of the path check\nThe shown server's sizes the warmup",
} as const;

export const preflightNote = (ms: string) =>
  `Preflight request ${ms} ms: connection setup plus the response\nNot a latency measurement`;
