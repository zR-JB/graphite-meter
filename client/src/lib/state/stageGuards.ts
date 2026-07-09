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

/** Latest sample for the currently measured one-way transfer phase. A warmup
 *  or a different transfer phase deliberately reads as 0 so the previous
 *  stage's last value cannot leak into the next gauge state. */
export function latestOneWayThroughputForPhase(
  phase: "download" | "upload",
  throughput: readonly ThroughputSample[],
): number {
  const last = throughput.at(-1);
  return last?.phase === phase ? last.bytesPerSec : 0;
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
  let seenDown = false;
  let seenUp = false;
  for (let i = throughput.length - 1; i >= 0; i--) {
    const s = throughput[i];
    if (s.phase !== "bidirectional") break;
    if (s.dir === "down" && !seenDown) {
      down = s.bytesPerSec;
      seenDown = true;
    } else if (s.dir === "up" && !seenUp) {
      up = s.bytesPerSec;
      seenUp = true;
    }
    if (seenDown && seenUp) break;
  }
  return { down, up };
}
