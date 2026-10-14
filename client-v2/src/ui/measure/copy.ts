// The instrument's own words: the dial's notes and the labels of the chips, lanes, cards, lens and toast.
import type { ShownPhase } from "../../app/run";
import { READINESS_TIP, TIP } from "../../view/words";

/** What the dial's note says between measurements; the explainer, when there is one, is the hint's tip. */
export const PHASE_HINT: Partial<Record<ShownPhase, { text: string; tip?: string }>> = {
  idle: { text: "Ready to measure your connection" },
  connecting: { text: "Verifying the selected paths", tip: READINESS_TIP.checking },
  warmup: { text: "Warming up; not counted", tip: TIP.warmup },
};

const listed = (names: readonly string[]) =>
  names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

export const DIAL = {
  restart: "Press Run again to restart",
  retry: "Press Run again to retry",
  unknownError: "Something went wrong",
  noData: (time: string) => `No data for ${time}`,
  noReplies: (time: string) => `No replies for ${time}`,
  checkFailed: "Connection check failed",
  /** The stages a finished run lacks: those with no result, else those some servers did not measure. */
  unfinished: (failed: readonly string[], partial: readonly string[]) =>
    failed.length ? `${listed(failed)} failed` : `${listed(partial)} measured in part`,
  unavailable: (throughput: boolean, latency: boolean) =>
    throughput && latency
      ? "Throughput and latency paths are unavailable"
      : throughput
        ? "Throughput path is unavailable"
        : "Latency path is unavailable",
  /** A bidirectional result that measured only one direction. */
  oneDirection: "One direction",
} as const;

export const CHIPS = {
  legend: "Test stages",
  lockedRunning: "Locked while it runs",
  lockedStarting: "Locked while the test starts",
  lockedUntilEnd: "Locked until the run ends",
} as const;

export const LANES = {
  label: "Latency, jitter and probe timeouts by phase",
  group: "Median, jitter, timeouts and spread by stage",
  heads: ["", "Median", "Jitter", "Timeouts", "", "Added"],
  baseline: "Baseline",
  picker: "Show latency to",
} as const;

export const CARDS = {
  wire: "wire",
  jitter: "jitter",
  noData: "no data",
  timeout: "timeout",
  probeTimeout: "probe timeout",
  downUp: "Down + up",
  issues: "Issues",
  rate: "Rate",
  idleOverTime: "Idle latency over time",
  overTime: (stage: string) => `${stage} over time`,
} as const;

export const LENS = "Servers shown in the results";

export const TOAST = {
  issue: "Issue",
  unsaved: "Not saved",
  issueOn: (server: string) => `Issue on ${server}`,
  many: (count: number) => `${count} measurement issues, each marked on its card`,
} as const;
