/* Adaptive RTT and probe-deadline estimator for the ping worker (RFC 6298-style). */

export interface RttEstimate {
  srtt: number;
  rttvar: number;
  haveRtt: boolean;
}

export const INITIAL_RTT_ESTIMATE: RttEstimate = {
  srtt: 0,
  rttvar: 0,
  haveRtt: false,
};

/* Fold an RTT sample into the SRTT/RTTVAR estimator (RFC 6298, α=1/8, β=1/4). */
export function observeRtt(prev: RttEstimate, rttMs: number): RttEstimate {
  if (!prev.haveRtt) return { srtt: rttMs, rttvar: rttMs / 2, haveRtt: true };
  return {
    srtt: 0.875 * prev.srtt + 0.125 * rttMs,
    rttvar: 0.75 * prev.rttvar + 0.25 * Math.abs(prev.srtt - rttMs),
    haveRtt: true,
  };
}

/** The latency channel's probe deadline; its ceiling is the ping timeout ceiling. */
export const PROBE_DEADLINE = { k: 4, floorMs: 250 } as const;

/* Adaptive probe deadline: RTO = SRTT + K·RTTVAR, clamped to [deadlineFloorMs, deadlineCeilMs]. */
export function probeDeadline(
  est: RttEstimate,
  deadlineK: number,
  deadlineFloorMs: number,
  deadlineCeilMs: number,
): number {
  if (!est.haveRtt) return deadlineFloorMs;
  const rto = est.srtt + deadlineK * Math.max(1, est.rttvar);
  return Math.min(Math.max(rto, deadlineFloorMs), deadlineCeilMs);
}
