// Pure helpers, outside the rune-based store so bun tests can import them
// without the Svelte runtime.
import type { ThroughputSample, TransportRole } from "../runner/contract";

const MEASURED_STAGE_ORDER = ["latency", "download", "upload"] as const;

export function canToggleMeasuredStage(
  stage: "latency" | "download" | "upload",
  isRunning: boolean,
  phaseStage: TransportRole | null,
): boolean {
  if (!isRunning) return true;

  const currentIndex = MEASURED_STAGE_ORDER.indexOf(
    phaseStage as (typeof MEASURED_STAGE_ORDER)[number],
  );
  const stageIndex = MEASURED_STAGE_ORDER.indexOf(stage);

  return currentIndex >= 0 && stageIndex > currentIndex;
}

export function canDisableBidirectional(
  phaseStage: TransportRole | null,
  isRunning: boolean,
): boolean {
  if (!isRunning) return true;
  return phaseStage !== "bidirectional";
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

export function updateLiveThroughput(
  current: readonly ThroughputSample[],
  sample: ThroughputSample,
): ThroughputSample[] {
  const samePhase = current[0]?.phase === sample.phase ? current : [];
  return [...samePhase.filter((value) => value.dir !== sample.dir), sample];
}

export function sustainedRate(
  samples: readonly Pick<ThroughputSample, "t" | "bytesPerSec">[],
  dwellTargetMs: number,
): number {
  if (samples.length < 2) return samples[0]?.bytesPerSec ?? 0;
  const weighted = samples.map((sample, index) => ({
    bytesPerSec: sample.bytesPerSec,
    dwellMs: Math.max(
      1,
      index === 0 ? samples[1].t - sample.t : sample.t - samples[index - 1].t,
    ),
  }));
  weighted.sort((a, b) => b.bytesPerSec - a.bytesPerSec);
  let dwellMs = 0;
  for (const sample of weighted) {
    dwellMs += sample.dwellMs;
    if (dwellMs >= dwellTargetMs) return sample.bytesPerSec;
  }
  return weighted.at(-1)!.bytesPerSec;
}
