/* ============================================================
 * RealBackend pure helpers — origin/URL mapping, small math, and stage-
 * activity queries with no fetch/worker/websocket entanglement. Split out
 * of RealRunner.ts (mirrors laneBudget.ts) so they're unit-testable without
 * pulling in RealRunner.ts's build-time BUILD defines.
 * ============================================================ */

import type { RunnerConfig, PhaseActivity } from "../contract";

/** Resolve the fetch base URL for the backend. `host:"auto"` (or empty) means
 *  same-origin (relative requests) — the Stage-1 case where the Go server serves
 *  both the app and the API. A concrete host builds an absolute origin. */
export function resolveBase(endpoint?: RunnerConfig["endpoint"]): string {
  if (!endpoint || endpoint.host === "auto" || endpoint.host === "") return "";
  const scheme = endpoint.port === 443 ? "https" : "http";
  return `${scheme}://${endpoint.host}:${endpoint.port}`;
}

/** Map an http(s) origin to its ws(s) equivalent for the latency bus. Anything
 *  already ws(s):// (or relative) passes through unchanged. */
export function httpToWs(origin: string): string {
  if (origin.startsWith("https://"))
    return "wss://" + origin.slice("https://".length);
  if (origin.startsWith("http://"))
    return "ws://" + origin.slice("http://".length);
  return origin;
}

/** Upgrade a ws:// base to wss://, unchanged otherwise. Used to force the
 *  latency bus encrypted when the page itself loaded over https, regardless
 *  of what scheme the server-advertised origin guessed at. */
export function wsToWss(base: string): string {
  return base.startsWith("ws://")
    ? "wss://" + base.slice("ws://".length)
    : base;
}

/** Median of a non-empty number list (used for the pre-test ping). */
export function median(xs: number[]): number {
  const sorted = [...xs].sort((a, b) => a - b);
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Whether a stage runs a ping channel: the idle latency stage, or a transfer
 *  stage with loaded latency (bufferbloat) active. */
export function needsPings(activity: PhaseActivity): boolean {
  return (
    activity.stage === "latency" ||
    (activity.transfer.length > 0 && activity.loadedLatency)
  );
}

/** Per-lane spawn delay for `streams` parallel lanes over a `warmupMs`
 *  warmup window, capped at `baseMs` (RealRunner's LANE_STAGGER_MS) but
 *  shrunk so even the last lane (index streams-1) still starts within half
 *  the warmup. Zero (spawn together) for a single lane or no warmup. */
export function laneStaggerMs(
  streams: number,
  warmupMs: number,
  baseMs: number,
): number {
  return streams > 1
    ? Math.min(baseMs, Math.floor((warmupMs * 0.5) / (streams - 1)))
    : 0;
}

export interface ServerSnapshot {
  n: number;
  t: number;
}

/** Append one cumulative server snapshot and derive a live rate over approximately
 *  the requested server-time window. Bytes and time never cross clock domains. */
export function serverRateWindow(
  samples: ServerSnapshot[],
  sample: ServerSnapshot,
  windowNanos: number,
): { samples: ServerSnapshot[]; bytesPerSec: number } {
  const next = [...samples, sample];
  const cutoff = sample.t - windowNanos;
  let first = 0;
  while (first + 1 < next.length && next[first + 1].t <= cutoff) first++;
  const window = first ? next.slice(first) : next;
  const start = window[0];
  const seconds = start ? (sample.t - start.t) / 1e9 : 0;
  return {
    samples: window,
    bytesPerSec: seconds > 0 ? (sample.n - start.n) / seconds : 0,
  };
}
