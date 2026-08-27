import type { ThroughputSample, TransportRole } from "../runner/contract";

const MEASURED_STAGE_ORDER = ["latency", "download", "upload"] as const;

export function canToggleMeasuredStage(
  stage: "latency" | "download" | "upload",
  isRunning: boolean,
  phaseStage: TransportRole | null,
): boolean {
  if (!isRunning) return true;
  if (phaseStage == null) return false;
  const current = MEASURED_STAGE_ORDER.indexOf(
    phaseStage as (typeof MEASURED_STAGE_ORDER)[number],
  );
  return current >= 0 && MEASURED_STAGE_ORDER.indexOf(stage) > current;
}

export function canDisableBidirectional(
  phaseStage: TransportRole | null,
  isRunning: boolean,
): boolean {
  if (!isRunning) return true;
  return phaseStage !== "bidirectional";
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
  const lanes = { down: 0, up: 0 };
  const seen = { down: false, up: false };
  for (let i = throughput.length - 1; i >= 0; i--) {
    const sample = throughput[i];
    if (sample.phase !== "bidirectional") break;
    if (!seen[sample.dir]) {
      lanes[sample.dir] = sample.bytesPerSec;
      seen[sample.dir] = true;
    }
    if (seen.down && seen.up) break;
  }
  return lanes;
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
