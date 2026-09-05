/* Canonical ping outcome crossing the worker boundary. */
export interface PingSample {
  rtt: number;
  lost: boolean;
  /** Probe submission time; determines membership at a stage's stop boundary. */
  sentAtEpochMs?: number;
  /** Reply receipt time or timeout deadline in the worker performance epoch. */
  observedAtEpochMs: number;
}

export type PingInterruptionReason = "unresolved" | "send-failed";

export type PingWorkerEvent =
  | { type: "open" | "ready" | "stopped" | "resume" | "auth-required" }
  | { type: "samples"; samples: PingSample[] }
  | {
      type: "interrupted";
      sentAtEpochMs: number[];
      reason: PingInterruptionReason;
    }
  | { type: "stall"; detail: string };

export const PING_TIMEOUT_CEIL_MS = 10_000;
export const PING_STOP_MARGIN_MS = 250;

export function pingSample(
  rtt: number,
  lost: boolean,
  observedAtMs = performance.now(),
  timeOriginMs = performance.timeOrigin,
): PingSample {
  return {
    rtt,
    lost,
    observedAtEpochMs: timeOriginMs + observedAtMs,
  };
}

/* Performance time origins are epoch-based and therefore remain comparable even when the worker and window have. */
export function pingSampleContextTime(
  sample: PingSample,
  timeOriginMs = performance.timeOrigin,
): number {
  return sample.observedAtEpochMs - timeOriginMs;
}
