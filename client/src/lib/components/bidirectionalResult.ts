import type { RunResult, ThroughputResult } from "../runner/contract";

type BidirectionalResult = RunResult["bidirectional"];

/* Terminal bidirectional presentation retains one qualifying lane as a diagnostic, but reserves its combined. */
export function bidirectionalResultPresentation(result: BidirectionalResult): {
  down: ThroughputResult | null;
  up: ThroughputResult | null;
  combinedBytesPerSec: number | null;
  survivingDirection: "down" | "up" | null;
} {
  const down = result?.down ?? null;
  const up = result?.up ?? null;
  if (down && up)
    return {
      down,
      up,
      combinedBytesPerSec: down.reportedBytesPerSec + up.reportedBytesPerSec,
      survivingDirection: null,
    };
  return {
    down,
    up,
    combinedBytesPerSec: null,
    survivingDirection: down ? "down" : up ? "up" : null,
  };
}
