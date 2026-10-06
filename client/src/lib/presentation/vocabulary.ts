// User-facing names shared by every browser view.
import type { IconName } from "./icons";
import type {
  ConnectivityState,
  FailureReason,
  Phase,
  PingCadence,
  RunResult,
  StageStatus,
  TransportKind,
  TransportRole,
} from "../runner/contract";
import type { ConnectionValidationState } from "../runner/paths";
import type { ThemePref } from "../state/persistence";
import type { PreparationState } from "../state/store.svelte";

export const MISSING = "—";

export const STAGE: Record<
  TransportRole,
  { label: string; short: string; icon: IconName; about: string }
> = {
  latency: {
    label: "Latency",
    short: "Latency",
    icon: "ping",
    about: "Round trips on an idle connection",
  },
  download: {
    label: "Download",
    short: "Download",
    icon: "download",
    about: "Server to this browser",
  },
  upload: {
    label: "Upload",
    short: "Upload",
    icon: "upload",
    about: "This browser to the server",
  },
  bidirectional: {
    label: "Bidirectional",
    short: "Bi-dir",
    icon: "bidirectional",
    about: "Download and upload at the same time",
  },
};

export const LATENCY_POPULATION: Record<
  TransportRole,
  { label: string; short: string }
> = {
  latency: { label: "Idle latency", short: "Idle" },
  download: { label: "Loaded latency · Download", short: "Loaded down" },
  upload: { label: "Loaded latency · Upload", short: "Loaded up" },
  bidirectional: {
    label: "Loaded latency · Bidirectional",
    short: "Loaded bi-dir",
  },
};

const PHASE: Record<Phase, string> = {
  idle: "Not started",
  connecting: "Checking paths",
  warmup: "Warmup",
  latency: "Latency",
  download: "Download",
  upload: "Upload",
  bidirectional: "Bidirectional",
  complete: "Complete",
  aborted: "Stopped",
  error: "Failed",
};

export type Tone = "ok" | "brand" | "warn" | "err" | "neutral";

export const BLOCKED = "Test cannot start";
export const counted = (count: number, noun: string) =>
  `${count} ${count === 1 ? noun : `${noun}s`}`;
type Readiness = ConnectionValidationState | "sign-in" | "blocked";
export const READINESS: Record<Readiness, { label: string; tone: Tone }> = {
  verified: { label: "Ready", tone: "ok" },
  checking: { label: "Checking", tone: "brand" },
  stale: { label: "Recheck needed", tone: "warn" },
  failed: { label: "Failed", tone: "err" },
  "sign-in": { label: "Sign in", tone: "warn" },
  blocked: { label: BLOCKED, tone: "warn" },
};
/** A verified path while a run holds it, in Settings and Details alike. */
export const IN_USE = { label: "In use", tone: "brand" } as const;
export const READINESS_TIP: Record<Exclude<Readiness, "blocked">, string> = {
  verified:
    "Ready\nThe selected servers and paths passed their check\n" +
    "Recent checks are reused; expired ones rerun before a test",
  checking: "Checking\nAsking each selected server which paths it offers",
  stale:
    "Recheck needed\nThe last check expired or a path changed\nIt reruns before the test",
  failed: "Failed\nA selected server or path did not answer its check",
  "sign-in": "Sign in\nA selected server only admits signed-in clients",
};

export const START_FAILED = "Test could not start";

export const STATUS = {
  complete: "Complete",
  partial: "Partial",
  failed: "Failed",
  "not-run": "Skipped",
  running: "Running",
  recovering: "Recovering",
  upcoming: "Upcoming",
  next: "Next run",
  stopped: PHASE.aborted,
} as const satisfies Record<StageStatus, string> & Record<string, string>;

/** One tone per status and outcome on every surface; the rest stay neutral. */
export const STATUS_TONE = {
  partial: "warn",
  incomplete: "warn",
  failed: "err",
  recovering: "warn",
  stopped: "neutral",
  "not-run": "neutral",
} as const satisfies Partial<Record<keyof typeof STATUS | Outcome, Tone>>;

/** A stalled run is "Recovering" wherever it shows: footer, toast, stage and topbar. */
export const CONNECTIVITY: Record<
  ConnectivityState | "checking" | "recovering",
  { label: string; tone: Tone }
> = {
  connected: { label: "Connected", tone: "ok" },
  degraded: { label: "Degraded", tone: "warn" },
  unstable: { label: "Unstable", tone: "err" },
  offline: { label: "Offline", tone: "err" },
  checking: { label: "Checking", tone: "neutral" },
  recovering: { label: STATUS.recovering, tone: STATUS_TONE.recovering },
};

export const stageStatusLabel = (status: StageStatus) =>
  status === "complete" ? MISSING : STATUS[status];

const FAILURE: Record<FailureReason, string> = {
  "preparation-failed": "Couldn't prepare the connection",
  "connection-lost": "Connection lost",
  timeout: "Stopped delivering data",
  "sign-in-required": "Sign-in required",
  "server-busy": "Server at capacity",
  "protocol-error": "Unexpected server response",
  "insufficient-evidence": "Too little measured time",
};
export const reasonLabel = (reason: FailureReason) =>
  FAILURE[reason] ?? "Measurement issue";

/** Explainers, set apart by their title line: a title and at most two short lines; definitions in full live in
    docs/MEASUREMENTS.md. */
export const JARGON = {
  download:
    "Download\nPayload bytes received per second\nMean of the last interval, warmup left out",
  upload:
    "Upload\nPayload bytes the server received per second\nTimed at the server; queued bytes don't count",
  bidirectional: "Bidirectional\nDownload and upload at once, added together",
  latency:
    "Idle latency\nMedian round trip on an idle connection\nThe base for added latency",
  loadedLatency: "Loaded latency\nHighest median round trip during a transfer",
  transferred: "Transferred\nPayload bytes measured in this stage",
  peak: "Peak\nHighest mean over windows of 0.5 s or more\nNever below the headline",
  rateStability:
    "Stability\n100% minus the spread of the 250 ms rates\nSpread: standard deviation over the mean",
  noData: "No data\nNo bytes arrived; it counts in the average",
  noReplies: "No replies\nThese probes timed out; not packet loss",
  replies: "Replies\nIdle probes answered in this stage",
  latencyStability: "Stability\n100% minus jitter as a share of the median",
  addedLatency: "Added latency\nLoaded minus idle median, same server",
  jitter:
    "Jitter\nMean change between consecutive replies\nTimeouts are left out",
  latencyMedian: "Median\nHalf the replies were faster, half slower",
  latencyRange: "Range\nFastest to slowest reply",
  keyShortcuts:
    "Keyboard shortcuts\nS, D, H, R and T act when no field has focus\nTurn off if speech input triggers them",
  wireRate:
    "Wire rate\nPayload plus the protocol headers carried with it\nAn estimate; hover a result for its parts",
  rateUnit:
    "Rate unit\nBits: Mbit/s, as internet plans\nBytes: MB/s, 8× lower, as downloads",
  unitPrefix:
    "Prefix\nDecimal: steps of 1,000 (k, M, G)\nBinary: steps of 1,024 (Ki, Mi, Gi)",
  throughputPath:
    "Throughput path\nTransport and HTTP version of the test bytes",
  latencyPath: "Latency path\nTransport of the probes, on its own connection",
  pathEvidence: "Evidence\nThe HTTP version each side observed",
  uploadFeed: "Upload feed\nHow the server reports upload bytes back",
  clientAddress: "Your address\nAs the server saw it: proxy header or socket",
  serverLoad:
    "Load\nTests on the server when the path was checked\nPast half its slots, they share the bandwidth",
  serverInstance: "Server\nEngine version; the instance changes on restart",
  probeAccounting:
    "Probe accounting\nA timeout: no reply by the probe's deadline\nUnfinished or failed sends are not timeouts",
  pretestLatency:
    "Pre-test latency\nMedian round trip of the path check\nThe shown server's sizes the warmup",
  testServers:
    "Test servers\nUp to 4; their speeds add up\nAll are probed; the lanes show one at a time",
  warmup:
    "Warmup\nOpens connections before each stage\n10 round trips to 4 s; never counted",
  stageTime:
    "Duration\nEach stage's planned length\nUp to the servers' limit, 5 min by default",
  bidirectionalStage:
    "Bidirectional stage\nDownload and upload at once, after the others",
  earlyFinish:
    "Early finish\nEnds a steady stage after about half its time\nNever after a stall, gap or lost server",
  saveResults:
    "Save results\nKeeps the newest 2,000 runs in this browser\nNothing is uploaded",
  gaugeAuto:
    "Automatic scale\nThe chart follows the peak\nThe gauge steps 1, 2, 5, 10 of its unit",
  gaugeMax:
    "Maximum\nFixes the chart ceiling\nThe gauge rounds up to a 1, 2, 5 step",
  idleCadence:
    "Idle latency cadence\nReply: the next probe leaves on each reply\nFast 80 ms · Medium 250 ms · Slow 600 ms",
  loadedCadence:
    "Loaded latency cadence\nProbes during transfers, start to start\nFast 80 ms · Medium 250 ms · Slow 600 ms",
  skipLoadedLatency:
    "Skip loaded latency\nWith the Latency stage off, transfers send no probes",
  datagramThroughput:
    "Datagram throughput\nAdds WebTransport datagrams as a path\nNever resent; expect lower rates than streams",
  forcedStreams:
    "Streams\nParallel connections per server and direction\nForced opens the exact count, past browser limits",
  autoStreamCount:
    "HTTP/1.1 stream limit\nParallel HTTP/1.1 requests per direction\nHTTP/2 and HTTP/3 choose their own",
  forcedStreamCount:
    "Streams per server and direction\nOpens exactly this many\nWebTransport carries at most 16 per session",
} as const;

export const PHASE_HINT: Partial<
  Record<Phase, { text: string; tip?: string }>
> = {
  idle: { text: "Ready to measure your connection" },
  connecting: {
    text: "Verifying the selected paths",
    tip: READINESS_TIP.checking,
  },
  warmup: { text: "Warming up; not counted", tip: JARGON.warmup },
};

export const preflightNote = (ms: string) =>
  `Preflight request ${ms} ms: connection setup plus the response\nNot a latency measurement`;

export type Outcome = NonNullable<RunResult["outcome"]>;
export const OUTCOME: Record<Outcome, string> = {
  complete: "Complete",
  partial: "Partial",
  incomplete: "Incomplete",
};

export const phaseLabel = (phase: Phase, outcome: Outcome = "complete") =>
  phase === "complete" ? OUTCOME[outcome] : PHASE[phase];

const CHECKING_SIGN_IN = "Checking sign-in";

export function statusLabel(
  preparation: PreparationState["status"],
  phase: Phase,
  outcome?: Outcome,
): string {
  if (preparation === "blocked") return BLOCKED;
  if (preparation === "failed") return START_FAILED;
  if (preparation === "authenticating") return CHECKING_SIGN_IN;
  if (preparation === "checking" || preparation === "launching")
    return PHASE.connecting;
  return phaseLabel(phase, outcome);
}

/** A path choice's second line: what it does, in a few words; availability stays in its tip. */
export const PATH_NOTE: Record<
  "throughput" | "latency",
  Record<string, string>
> = {
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

/** Bare "webtransport" names the session: streams carry throughput, datagrams latency. */
export const TRANSPORT: Record<TransportKind, string> = {
  "fetch-stream": "Fetch streams",
  websocket: "WebSocket",
  webtransport: "WebTransport streams",
  "webtransport-datagram": "WebTransport datagrams",
};
export const transportLabel = (
  kind: TransportKind,
  role: "throughput" | "latency",
) =>
  TRANSPORT[
    kind === "webtransport" && role === "latency"
      ? "webtransport-datagram"
      : kind
  ];

export const THEME: Record<ThemePref, { label: string; icon: IconName }> = {
  light: { label: "Light", icon: "sun" },
  dark: { label: "Dark", icon: "moon" },
  auto: { label: "Auto", icon: "contrast" },
};

export const resolvedPhase = (phase: Phase) =>
  phase === "complete" || phase === "aborted" || phase === "error";

export const RUN_ACTION = {
  start: "Start test",
  stop: "Stop test",
  again: "Run again",
  cancel: "Cancel",
} as const;

export function runActionLabel(
  preparing: boolean,
  running: boolean,
  phase: Phase,
) {
  if (preparing) return RUN_ACTION.cancel;
  if (running) return RUN_ACTION.stop;
  return resolvedPhase(phase) ? RUN_ACTION.again : RUN_ACTION.start;
}

export const PING_CADENCE: Record<PingCadence, string> = {
  "reply-driven": "Reply-driven",
  fast: "Fast (80 ms)",
  medium: "Medium (250 ms)",
  slow: "Slow (600 ms)",
};
/** The cadence's word on a segment; its interval is in the row's tip and the segment's name. */
export const PING_CADENCE_SHORT: Record<PingCadence, string> = {
  "reply-driven": "Reply",
  fast: "Fast",
  medium: "Medium",
  slow: "Slow",
};
