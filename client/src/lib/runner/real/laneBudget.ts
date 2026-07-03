/* ============================================================
 * Lane budget — how many parallel POST/GET streams a transfer direction
 * gets, carved from the browser's per-origin connection pool. Pure so it's
 * unit-testable without spinning up RealBackend/workers; RealRunner.ts calls
 * this once per direction at prime time (see #laneBudget).
 * ============================================================ */

import type { FlowDirection, TransportKind } from "../contract";

/** The browser's ~6-connections-per-origin limit — the pool lanes are carved
 *  from on a non-multiplexed transport (fetch-stream/HTTP-1.1). */
export const BROWSER_CONN_BUDGET = 6;

export interface LaneBudgetOptions {
  kind: TransportKind;
  /** The stage's activity.transfer — `["down"]`, `["up"]`, or `["down","up"]`
   *  for bidirectional. Order is fixed by schedule.ts's activityFor. */
  transfer: FlowDirection[];
  /** Which direction this call is sizing lanes for. */
  dir: FlowDirection;
  /** Whether a ping/latency bus needs its own connection this phase. */
  needsPing: boolean;
  /** The advanced `parallelStreams` ceiling (#laneCeiling) — an upper bound
   *  applied PER DIRECTION, identical to how a standalone download/upload
   *  stage is capped. Never applied to a combined down+up total. */
  ceiling: number;
  /** Override for tests; defaults to the real browser budget. */
  totalBudget?: number;
}

/** Lanes to open for `dir` this phase. On a multiplexed transport every lane
 *  shares one congestion window, so extra lanes buy nothing — cap low. On
 *  fetch-stream, each lane is its own TCP connection: reserve the buses this
 *  phase needs (ping, upload-progress), then split what's left. A
 *  bidirectional stage splits the remaining budget 50/50 between its two
 *  concurrent pools (odd remainder → "down", `transfer[0]`) BEFORE applying
 *  the ceiling, so `parallelStreams` still means "cap per direction" exactly
 *  as it does for a standalone download/upload stage — never "cap the
 *  combined total". */
export function laneBudget(opts: LaneBudgetOptions): number {
  if (opts.kind !== "fetch-stream")
    return Math.max(1, Math.min(2, opts.ceiling));

  const total = opts.totalBudget ?? BROWSER_CONN_BUDGET;
  const buses =
    (opts.needsPing ? 1 : 0) + (opts.transfer.includes("up") ? 1 : 0);
  const budget = Math.max(1, total - buses);

  if (opts.transfer.length > 1) {
    const half = Math.floor(budget / 2);
    const share = opts.dir === opts.transfer[0] ? budget - half : half;
    return Math.max(1, Math.min(share, opts.ceiling));
  }

  return Math.max(1, Math.min(budget, opts.ceiling));
}
