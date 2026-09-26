// User-facing names shared by every view; the TUI uses the same table.
import type { IconName } from "./icons";
import type {
  Phase,
  PingCadence,
  RunResult,
  TransportKind,
  TransportRole,
} from "../runner/contract";
import type { ConnectionValidationState } from "../runner/paths";
import type { ThemePref } from "../state/persistence";

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
  ConnectionValidationState,
  { label: string; tone: "ok" | "brand" | "warn" | "err" }
> = {
  verified: { label: "Ready", tone: "ok" },
  checking: { label: "Checking", tone: "brand" },
  stale: { label: "Recheck needed", tone: "warn" },
  failed: { label: "Failed", tone: "err" },
};

export const BLOCKED = "Test cannot start";

export type Outcome = NonNullable<RunResult["outcome"]>;
export const OUTCOME: Record<Outcome, string> = {
  complete: "Complete",
  partial: "Partial",
  incomplete: "Incomplete",
};

export const phaseLabel = (phase: Phase, outcome: Outcome = "complete") =>
  phase === "complete" ? OUTCOME[outcome] : PHASE[phase];

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

export function runActionLabel(
  preparing: boolean,
  running: boolean,
  phase: Phase,
) {
  if (preparing) return "Cancel";
  if (running) return "Stop test";
  return resolvedPhase(phase) ? "Run again" : "Start test";
}

export const PING_CADENCE: Record<PingCadence, string> = {
  "reply-driven": "Reply-driven",
  fast: "Fast (80 ms)",
  medium: "Medium (250 ms)",
  slow: "Slow (600 ms)",
};
