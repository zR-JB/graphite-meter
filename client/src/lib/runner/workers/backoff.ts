/* Reconnect backoff for latency probes and the HTTP upload progress feed. */

/* Next reconnect backoff (ms): starts at `minMs` on the first failure (prev 0), then doubles each subsequent. */
export function nextBackoff(
  prev: number,
  minMs: number,
  maxMs: number,
): number {
  return prev === 0 ? minMs : Math.min(prev * 2, maxMs);
}
