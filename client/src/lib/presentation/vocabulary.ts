// User-facing names shared by every view; the TUI uses the same table.
import { ICON } from "../constants";
import type {
  Phase,
  PingCadence,
  RunResult,
  TransportRole,
} from "../runner/contract";

export const MISSING = "—";

export const STAGE: Record<
  TransportRole,
  { label: string; short: string; icon: string }
> = {
  latency: { label: "Latency", short: "Latency", icon: ICON.ping },
  download: { label: "Download", short: "Download", icon: ICON.download },
  upload: { label: "Upload", short: "Upload", icon: ICON.upload },
  bidirectional: {
    label: "Bidirectional",
    short: "Bi-dir",
    icon: ICON.bidirectional,
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
  idle: "Ready",
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

export type Outcome = NonNullable<RunResult["outcome"]>;
export const OUTCOME: Record<Outcome, string> = {
  complete: "Complete",
  partial: "Partial",
  incomplete: "Incomplete",
};

export const phaseLabel = (phase: Phase, outcome: Outcome = "complete") =>
  phase === "complete" ? OUTCOME[outcome] : PHASE[phase];

export const PING_CADENCE: Record<PingCadence, string> = {
  "reply-driven": "Reply-driven",
  fast: "Fast (80 ms)",
  medium: "Medium (250 ms)",
  slow: "Slow (600 ms)",
};
