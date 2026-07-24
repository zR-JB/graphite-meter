import type { Phase, ThroughputSample } from "../runner/contract";

// Pure helpers, kept outside the rune-based store so bun tests can import them
// without the Svelte runtime.
export function canDisableBidirectional(
  phase: Phase,
  isRunning: boolean,
): boolean {
  // Bidirectional runs last. It can be removed until its own phase has started;
  // re-enabling is a Settings-only action.
  if (!isRunning) return true;
  return phase !== "bidirectional";
}

// Guard against a previous transfer's last sample leaking into the current gauge.
export function latestOneWayThroughputForPhase(
  phase: "download" | "upload",
  throughput: readonly ThroughputSample[],
): number {
  const last = throughput.at(-1);
  return last?.phase === phase ? last.bytesPerSec : 0;
}

// During bidirectional, the latest down/up samples arrive independently. Walk
// backward only through the current bidirectional tail and combine one of each.
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
