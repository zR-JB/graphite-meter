// The instrument's words the shared vocabulary does not hold: its explainers, the dial's notes, the run key, and the
// labels of the chips, lanes, cards, toast and connection dot. Each explainer is a title line and at most two more.
import type { Preparation, ShownPhase } from "../../app/run";
import type { Outcome } from "../../engine/run/result";
import { OUTCOME, PHASE, READINESS_TIP } from "../../view/words";

export const JARGON = {
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
} as const;

/** What the dial's note says between measurements; the explainer, when there is one, is the hint's tip. */
export const PHASE_HINT: Partial<Record<ShownPhase, { text: string; tip?: string }>> = {
  idle: { text: "Ready to measure your connection" },
  connecting: { text: "Verifying the selected paths", tip: READINESS_TIP.checking },
  warmup: { text: "Warming up; not counted", tip: JARGON.warmup },
};

export const DIAL = {
  restart: "Press Run again to restart",
  retry: "Press Run again to retry",
  unknownError: "Something went wrong",
  noData: (time: string) => `No data for ${time}`,
  noReplies: (time: string) => `No replies for ${time}`,
  checkFailed: "Connection check failed",
  unavailable: (throughput: boolean, latency: boolean) =>
    throughput && latency
      ? "Throughput and latency paths are unavailable"
      : throughput
        ? "Throughput path is unavailable"
        : "Latency path is unavailable",
  /** A bidirectional result that measured only one direction. */
  oneDirection: "One direction",
} as const;

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
  issueOn: (server: string) => `Issue on ${server}`,
  many: (count: number) => `${count} measurement issues, each marked on its card`,
} as const;
