// The instrument's own words: the dial's notes and the labels of the chips, lanes, cards, lens and toast.
import type { ShownPhase } from "../../app/run";
import { PHASE, READINESS_TIP, TIP } from "../../view/words";

/** What the dial's note says between measurements; the explainer, when there is one, is the hint's tip. */
export const PHASE_HINT: Partial<
  Record<ShownPhase, { status?: string; text: string; tip?: string }>
> = {
  idle: { text: "Ready to measure your connection" },
  connecting: { text: "Verifying the selected paths", tip: READINESS_TIP.checking },
  warmup: { status: PHASE.warmup, text: "Not counted", tip: TIP.warmup },
};

export const DIAL = {
  otherTab: "A test is running in another tab. This one starts when it ends.",
  /** A stop is never saved. */
  unsaved: "Nothing was saved",
  noData: (time: string) => `No data for ${time}`,
  noReplies: (time: string) => `No replies for ${time}`,
  /** A finished run's issues on more servers than one line names. */
  troubled: (servers: number) => `${servers} servers with issues`,
  more: (servers: number) => `and ${servers} more ${servers === 1 ? "server" : "servers"}`,
  unavailable: (throughput: boolean, latency: boolean) =>
    throughput && latency
      ? "Throughput and latency paths are unavailable"
      : throughput
        ? "Throughput path is unavailable"
        : "Latency path is unavailable",
} as const;

export const CHIPS = {
  legend: "Test stages",
  lockedRunning: "Locked while the test runs",
  lockedStarting: "Locked while the test starts",
  lockedUntilEnd: "Locked until the test ends",
} as const;

export const LANES = {
  label: "Latency, jitter and probe timeouts by stage",
  group: "Median, jitter, timeouts and spread by stage",
  heads: ["", "Median", "Jitter", "Timeouts", "", "Added"],
  picker: "Show latency to",
} as const;

export const FACT = {
  peak: { label: "Peak", tip: TIP.peak },
  stability: { label: "Stability", tip: TIP.rateStability },
  transferred: { label: "Transferred", tip: TIP.transferred },
  range: { label: "Range", tip: TIP.latencyRange },
  replies: { label: "Replies", tip: TIP.replies },
  timeouts: { label: "Timeouts", tip: TIP.timeouts },
  combined: { label: "Down + up", tip: "" },
} as const;
export type FactKey = keyof typeof FACT;

export const CARDS = {
  wire: "Wire rate",
  jitter: "Jitter",
  noData: "No data for",
  timeout: "timeout",
  probeTimeout: "Probe timeout",
  rate: "Rate",
  idleOverTime: "Idle latency over time",
  overTime: (stage: string) => `${stage} over time`,
} as const;

export const LENS = "Servers shown in the results";

export const TOAST = {
  unsaved: "Not saved",
} as const;
