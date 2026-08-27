import type { ThroughputSample } from "../runner/contract";
/* A throughput discontinuity is explicit runner lifecycle state, not a delay in presentation delivery or an. */
export function throughputSamplesContinuous(
  left: ThroughputSample,
  right: ThroughputSample,
): boolean {
  return left.continuityId === right.continuityId;
}
