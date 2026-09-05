/* Canonical ping outcome crossing the worker boundary. */
export interface PingSample {
  rtt: number;
  lost: boolean;
  /** Negotiated server application handling interval; milliseconds, same reply as RTT. */
  reflectorHandlingMs?: number;
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

/* Epoch-based performance coordinates remain comparable when the worker and window
 * have different time origins; subtract the receiving realm's origin to translate. */
export function pingSampleContextTime(
  sample: PingSample,
  timeOriginMs = performance.timeOrigin,
): number {
  return sample.observedAtEpochMs - timeOriginMs;
}

/** The codec has validated uint64 digits. Impossible or imprecise clock pairs
 * retain raw RTT but supply no adjusted diagnostic; never clamp them to zero. */
export function reflectorHandlingMs(
  rawRttMs: number,
  nanos: string | undefined,
): number | undefined {
  if (nanos === undefined) return undefined;
  const value = Number(nanos);
  const ms = value / 1_000_000;
  return Number.isSafeInteger(value) &&
    value >= 0 &&
    Number.isFinite(rawRttMs) &&
    ms <= rawRttMs
    ? ms
    : undefined;
}
