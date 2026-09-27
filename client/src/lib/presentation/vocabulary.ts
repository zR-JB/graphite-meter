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
  { label: string; short: string; icon: IconName }
> = {
  latency: { label: "Latency", short: "Latency", icon: "ping" },
  download: { label: "Download", short: "Download", icon: "download" },
  upload: { label: "Upload", short: "Upload", icon: "upload" },
  bidirectional: {
    label: "Bidirectional",
    short: "Bi-dir",
    icon: "bidirectional",
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
export const READINESS: Record<
  ConnectionValidationState | "sign-in" | "blocked",
  { label: string; tone: Tone }
> = {
  verified: { label: "Ready", tone: "ok" },
  checking: { label: "Checking", tone: "brand" },
  stale: { label: "Recheck needed", tone: "warn" },
  failed: { label: "Failed", tone: "err" },
  "sign-in": { label: "Sign in", tone: "warn" },
  blocked: { label: BLOCKED, tone: "warn" },
};

export const START_FAILED = "Test could not start";

/** Stage statuses, saved and live; the stage track adds its lock reasons. */
export const STATUS = {
  complete: "Complete",
  partial: "Partial",
  failed: "Failed",
  "not-run": "Skipped",
  running: "Running",
  recovering: "Recovering",
  upcoming: "Upcoming",
  next: "Next run",
} as const satisfies Record<StageStatus, string> & Record<string, string>;

/** One tone per status and outcome on every surface; the rest stay neutral. */
export const STATUS_TONE = {
  partial: "warn",
  incomplete: "warn",
  failed: "err",
  recovering: "warn",
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

/** A stage without a value shows its status; a complete one shows "—". */
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
    "Latency\nMedian round trip of probes on an idle connection\nThe base for added latency",
  transferred:
    "Transferred\nPayload bytes measured in this stage, each counted once",
  peak:
    "Peak\nHighest mean over any 0.5 s or longer window of the headline interval\n" +
    "Never below the headline",
  rateStability:
    "Stability\n100% minus the spread of 250 ms rates over the last 4 s\n" +
    "Spread: standard deviation divided by the mean",
  latencyStability: "Stability\n100% minus jitter as a share of the median",
  addedLatency:
    "Added latency\nLoaded median minus idle median, same server\nNegative: faster under load",
  jitter:
    "Jitter\nMean change between consecutive replies\nProbe timeouts are left out",
  wireRate:
    "Wire rate\nPayload rate plus the protocol headers the link also carried\n" +
    "An estimate from the path's framing and an assumed MTU",
  unitBits: "Bits per second (Mbit/s, Gbit/s), used by internet plans.",
  unitBytes: "MB/s or GB/s, used by download managers. One byte is eight bits.",
  unitDecimal: "Decimal prefixes: 1,000 per step (kbit/s, Mbit/s, Gbit/s).",
  unitBinary: "Binary prefixes: 1,024 per step (Kibit/s, Mibit/s, Gibit/s).",
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
    "Pre-test latency\nOne request before the test: connection setup plus the response\n" +
    "Not a latency measurement",
  checkReuse:
    "Recent successful checks are reused while the required server and path are unchanged. " +
    "Expired checks are refreshed before a test starts.",
  forcedStreams:
    "Streams\nParallel connections per server and direction\n" +
    "Automatic: chosen per protocol\nForced: the exact count, within shared connection limits",
  resetSettings:
    "Restore test, display, and history-saving settings to their defaults? " +
    "Your theme, panel layout, and saved results will be kept.",
} as const;

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

/** The footer and the gauge name one state: a refused start, preparation, else the phase. */
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
