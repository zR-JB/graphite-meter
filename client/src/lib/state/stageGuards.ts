import type { Phase, ThroughputSample } from "../runner/contract";

export function canDisableBidirectional(
  phase: Phase,
  isRunning: boolean,
): boolean {
  if (!isRunning) return true;
  return phase !== "bidirectional";
}

export function latestOneWayThroughputForPhase(
  phase: "download" | "upload",
  throughput: readonly ThroughputSample[],
): number {
  const last = throughput.at(-1);
  return last?.phase === phase ? last.bytesPerSec : 0;
}

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
