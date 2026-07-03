/* ============================================================
 * Pure stage-selection / bidirectional-lane logic, extracted from
 * AppStore (store.svelte.ts) so it is unit-testable under bun:test —
 * that file runs Svelte 5 rune calls ($state) at module scope, which
 * only exist under the Svelte compiler/runtime, so it can't be
 * imported directly in a plain test.
 * ============================================================ */

import type { Phase, ThroughputSample } from "../runner/contract";

/** Whether the bidirectional segment can be disabled from the stage track
 *  right now: freely while idle/complete/aborted/error, or while running as
 *  long as its own phase hasn't started yet (it always runs last — see
 *  schedule.ts). Re-enabling is Settings-only; this direction (off) has no
 *  symmetric "toggle on" case to guard. */
export function canDisableBidirectional(
  phase: Phase,
  isRunning: boolean,
): boolean {
  if (!isRunning) return true;
  return phase !== "bidirectional";
}

/** The bidirectional phase's two live lanes: the most recent down + up
 *  sample, scanning backward from the end of `throughput` until both are
 *  found or a differently-tagged sample is hit. `{down:0,up:0}` when neither
 *  lane has reported yet. */
export function latestBidirectionalLanes(
  throughput: readonly ThroughputSample[],
): { down: number; up: number } {
  let down = 0;
  let up = 0;
  for (let i = throughput.length - 1; i >= 0; i--) {
    const s = throughput[i];
    if (s.phase !== "bidirectional") break;
    if (s.dir === "down" && down === 0) down = s.bytesPerSec;
    else if (s.dir === "up" && up === 0) up = s.bytesPerSec;
    if (down > 0 && up > 0) break;
  }
  return { down, up };
}
