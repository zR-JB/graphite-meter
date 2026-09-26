// User-facing names shared by every view; the TUI uses the same table.
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

export const READINESS: Record<
  ConnectionValidationState | "sign-in",
  { label: string; tone: "ok" | "brand" | "warn" | "err" }
> = {
  verified: { label: "Ready", tone: "ok" },
  checking: { label: "Checking", tone: "brand" },
  stale: { label: "Recheck needed", tone: "warn" },
  failed: { label: "Failed", tone: "err" },
  "sign-in": { label: "Sign in", tone: "warn" },
};

export const BLOCKED = "Test cannot start";
export const START_FAILED = "Test could not start";

export const CONNECTIVITY: Record<ConnectivityState | "checking", string> = {
  connected: "Connected",
  degraded: "Degraded",
  unstable: "Unstable",
  offline: "Offline",
  checking: "Checking",
};

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

export const RECEIVER_TIMED = "receiver-timed";

export const JARGON = {
  addedLatency:
    "Added latency: loaded median minus idle median for the same server, signed. " +
    "The grade labels the worst stage: A ≤5 ms, B ≤30, C ≤60, D ≤200, otherwise F.",
  jitter:
    "Jitter: mean absolute change between consecutive replies. Lower is steadier; " +
    "probe timeouts are left out.",
  wireRate:
    "Estimated physical-link rate, including forward-path protocol overhead.",
  latency:
    "Median round-trip time (RTT) to the server and back. Lower is faster.",
  unitBits: "Bits per second (Mbit/s, Gbit/s), used by internet plans.",
  unitBytes: "MB/s or GB/s, used by download managers. One byte is eight bits.",
  unitDecimal: "Decimal prefixes: 1,000 per step (kbit/s, Mbit/s, Gbit/s).",
  unitBinary: "Binary prefixes: 1,024 per step (Kibit/s, Mibit/s, Gibit/s).",
} as const;

export type Outcome = NonNullable<RunResult["outcome"]>;
export const OUTCOME: Record<Outcome, string> = {
  complete: "Complete",
  partial: "Partial",
  incomplete: "Incomplete",
};

export const phaseLabel = (phase: Phase, outcome: Outcome = "complete") =>
  phase === "complete" ? OUTCOME[outcome] : PHASE[phase];

/** The footer and the gauge name one state: a refused start, preparation, else the phase. */
export function statusLabel(
  preparation: PreparationState["status"],
  phase: Phase,
  outcome?: Outcome,
): string {
  if (preparation === "blocked") return BLOCKED;
  if (preparation === "failed") return START_FAILED;
  if (preparation === "authenticating") return "Checking sign-in";
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
