/* Adaptive RTT/loss-timeout estimator for the ping worker (RFC 6298-style). */

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

/* Adaptive loss timeout: RTO = SRTT + K·RTTVAR, clamped to [lossFloorMs, lossCeilMs]. */
export function lossTimeout(
  est: RttEstimate,
  lossK: number,
  lossFloorMs: number,
  lossCeilMs: number,
): number {
  if (!est.haveRtt) return lossFloorMs;
  const rto = est.srtt + lossK * Math.max(1, est.rttvar);
  return Math.min(Math.max(rto, lossFloorMs), lossCeilMs);
}
