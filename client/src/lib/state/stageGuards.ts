// Pure helpers, outside the rune-based store so bun tests can import them
// without the Svelte runtime.
import type { Phase, ThroughputSample } from "../runner/contract";

export function canDisableBidirectional(
  phase: Phase,
  isRunning: boolean,
): boolean {
  // Bidirectional runs last. It can be removed until its own phase has started;
  // re-enabling is a Settings-only action.
  if (!isRunning) return true;
  return phase !== "bidirectional";
}

// A sample tagged with another phase must not leak into this phase's gauge.
export function latestOneWayThroughputForPhase(
  phase: "download" | "upload",
  throughput: readonly ThroughputSample[],
): number {
  const last = throughput.at(-1);
  return last?.phase === phase ? last.bytesPerSec : 0;
}

// Down and up samples arrive independently, so each lane takes its own newest
// value from the trailing run of bidirectional samples.
export function latestBidirectionalLanes(
  throughput: readonly ThroughputSample[],
): { down: number; up: number } {
  let down = 0;
  let up = 0;
  let seenDown = false;
  let seenUp = false;
  for (let i = throughput.length - 1; i >= 0; i--) {
    const sample = throughput[i];
    if (sample.phase !== "bidirectional") break;
    if (sample.dir === "down" && !seenDown) {
      down = sample.bytesPerSec;
      seenDown = true;
    } else if (sample.dir === "up" && !seenUp) {
      up = sample.bytesPerSec;
      seenUp = true;
    }
    if (seenDown && seenUp) break;
  }
  return { down, up };
}
