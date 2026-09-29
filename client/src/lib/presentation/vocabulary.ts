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

/** Explainers: a title line, then short lines; tooltips set the title apart. */
export const JARGON = {
  download:
    "Download\nPayload bytes the client received per second\n" +
    "Mean of the last interval with at least 0.8 s of evidence\nWarmup is left out",
  upload:
    "Upload\nPayload bytes the server received per second\n" +
    "Timed by the server's receiver, so queued bytes don't count\n" +
    "Mean of the last interval with at least 0.8 s of evidence",
  bidirectional:
    "Bidirectional\nDownload and upload at the same time, added together\n" +
    "Each direction is timed like its own stage",
  latency:
    "Idle latency\nMedian round trip of probes on an idle connection\nThe base for added latency",
  loadedLatency:
    "Loaded latency\nHighest median round trip while a transfer ran\n" +
    "Across download, upload and bidirectional",
  transferred:
    "Transferred\nPayload bytes measured in this stage, each counted once",
  peak:
    "Peak\nHighest mean of the headline window and of consecutive windows " +
    "of at least 0.5 s across its interval\nNever below the headline",
  rateStability:
    "Stability\n100% minus the coefficient of variation of the result's 250 ms rates\n" +
    "Coefficient of variation: standard deviation divided by the mean",
  noData:
    "No data\nTime in the result's window when no bytes arrived\n" +
    "It counts in the average: a stall is part of the link",
  noReplies:
    "No replies\nProbes sent in this time got no reply before the deadline\n" +
    "Each counts as a timeout, not as packet loss",
  latencyStability: "Stability\n100% minus jitter as a share of the median",
  addedLatency:
    "Added latency\nLoaded median minus idle median, same server\nNegative: faster under load",
  jitter:
    "Jitter\nMean absolute change between consecutive replies\nProbe timeouts are left out",
  latencyMedian: "Median\nHalf of the stage's replies were faster, half slower",
  latencyRange: "Range\nFastest to slowest reply in the stage",
  keyShortcuts:
    "Keyboard shortcuts\nS, D, H, R and T act on the page when no field has focus\n" +
    "Turn off if speech input or single keys trigger them",
  wireRate:
    "Wire rate\nPayload rate plus the protocol headers the link also carried\n" +
    "Ethernet, IP, TCP or QUIC, TLS and HTTP framing at a 1,500 B MTU\n" +
    "An estimate; a result's wire rate lists its parts on hover",
  rateUnit:
    "Rate unit\nBits: Mbit/s, the unit internet plans use\n" +
    "Bytes: MB/s, 8× lower, the unit downloads show",
  unitPrefix:
    "Prefix\nDecimal: 1,000 per step (k, M, G)\n" +
    "Binary: 1,024 per step (Ki, Mi, Gi), 4.6% lower at M",
  throughputPath:
    "Throughput path\nTransport and HTTP version that carry the test bytes\n" +
    "Verified before the test starts",
  latencyPath:
    "Latency path\nTransport the latency probes use, on its own connection",
  pathEvidence:
    "Evidence\nThe HTTP version the browser and the server each observed\n" +
    "Shown only where that side exposes it",
  uploadFeed:
    "Upload feed\nHow the server reports received upload bytes back to the page",
  clientAddress:
    "Your address\nThe address the server saw for this browser\n" +
    "From a trusted proxy header, else the socket peer",
  serverLoad:
    "Load\nTests running on the server when this path was checked\n" +
    "Past half its slots, other tests share the bandwidth being measured",
  serverInstance:
    "Server\nEngine version of the tested server\nThe instance changes when the backend restarts",
  probeAccounting:
    "Probe accounting\nReplies, and timeouts: no reply before the deadline\n" +
    "Unfinished probes and failed sends are counted apart, never as timeouts",
  preflight:
    "Preflight request\nOne request before any test: connection setup plus the response\n" +
    "Not a latency measurement",
  pretestLatency:
    "Pre-test latency\nMedian round trip of the probes that checked the latency path\n" +
    "The first selected server's sizes the warmup",
  testServers:
    "Test servers\nUp to 4 at once; their speeds are added together\n" +
    "They share this browser's connection\n" +
    "Each is probed for latency; the first one's is the result",
  warmup:
    "Warmup\nRuns before each stage to open its connections and ramp up\n" +
    "At least 10 round trips, at most 4 s; never counted",
  stageTime:
    "Duration\nEach stage's planned length; early finish can end one sooner\n" +
    "From 1 s up to the servers' stage limit, 5 min unless their operator raises it",
  bidirectionalStage:
    "Bidirectional stage\nDownload and upload at the same time, after the other stages\n" +
    "Its chip beside Start test can skip it; turn it back on here",
  earlyFinish:
    "Early finish\nEnds a stable stage after 52% of its time\n" +
    "Stable: score ≥ 0.86 over 4 s, held for 1.1 s\n" +
    "Needs 12 rate or 8 latency samples\nRate score: 1 − 2.2 × spread − 1.4 × drift\n" +
    "A stage with a stall, gap or lost server runs its full time",
  saveResults:
    "Save results\nKeeps complete, partial and incomplete runs in this browser\n" +
    "The newest 2,000 stay; nothing is uploaded",
  gaugeAuto:
    "Automatic scale\nThe chart follows the measured peak\n" +
    "The gauge starts at 1 Gbit/s and grows in powers of ten",
  gaugeMax:
    "Maximum\nFixes the chart ceiling\nThe gauge rounds up to the next power of ten",
  idleCadence:
    "Idle latency cadence\nHow often the Latency stage sends a probe\n" +
    "Reply-driven: the next probe leaves when the reply arrives",
  loadedCadence:
    "Loaded latency cadence\nHow often probes go out during transfers\n" +
    "Fixed times are start to start",
  skipLoadedLatency:
    "Skip loaded latency\nWith the Latency stage off, transfers send no probes\n" +
    "Off: transfers still measure loaded latency",
  datagramThroughput:
    "Datagram throughput\nAdds WebTransport datagrams as a throughput path\n" +
    "Datagrams are never resent; missing ones are not packet loss\n" +
    "Expect lower rates than streams, mostly for uploads",
  forcedStreams:
    "Streams\nParallel connections per server and direction\n" +
    "Automatic: chosen per protocol\nForced: the exact count, even past the browser's connection limit",
  autoStreamCount:
    "HTTP/1.1 stream limit\nCaps parallel HTTP/1.1 requests per direction\n" +
    "HTTP/2 and HTTP/3 choose their own count",
  forcedStreamCount:
    "Streams per server and direction\nOpens exactly this many requests\n" +
    "WebTransport carries at most 16 per session\nThe browser or server may queue or refuse more",
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
/** Characters of the longest statusLabel, so the footer never shifts. */
export const STATUS_LABEL_CH = Math.max(
  ...[
    ...Object.values(PHASE),
    ...Object.values(OUTCOME),
    BLOCKED,
    START_FAILED,
    CHECKING_SIGN_IN,
  ].map((label) => label.length),
);

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
