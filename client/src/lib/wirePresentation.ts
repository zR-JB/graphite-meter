import type { CompensationEstimate } from "./compensation";
import { compensationTransportLabel } from "./runner/protocol";

export type WirePresentation =
  | {
      kind: "estimate";
      text: string;
      lift: string;
      tooltip: string;
    }
  | {
      kind: "unavailable";
      text: string;
      tooltip: string;
    };

/** Own every piece of wire-estimate wording so all surfaces describe the same
 * model and exclusions. Measured goodput formatting stays with each caller. */
export function presentWireEstimate(
  estimate: CompensationEstimate,
  formatRate: (bytesPerSec: number) => string,
): WirePresentation {
  if (!estimate.available) {
    return {
      kind: "unavailable",
      text: "Loopback — no physical-wire estimate",
      tooltip:
        "Loopback traffic does not traverse a physical link, so no wire rate is modeled.",
    };
  }

  const central = formatRate(estimate.estimatedBytesPerSec);
  const lower = formatRate(estimate.lowerBytesPerSec);
  const upper = formatRate(estimate.upperBytesPerSec);
  const hasRange =
    Math.abs(estimate.upperBytesPerSec - estimate.lowerBytesPerSec) >
    Math.max(1, estimate.estimatedBytesPerSec * 0.0005);
  const assumptions = estimate.assumptions.length
    ? estimate.assumptions.join("; ")
    : "no additional framing assumptions";

  return {
    kind: "estimate",
    text: `≈ ${central} wire estimate`,
    lift: `+${((estimate.totalMultiplier - 1) * 100).toFixed(1)}%`,
    tooltip: [
      `Modeled wire rate: ${central}.`,
      hasRange
        ? `Modeled range: ${lower}–${upper}.`
        : "No modeled range for the selected assumptions.",
      `Multiplier: ×${estimate.totalMultiplier.toFixed(4)}.`,
      `Profile: ${estimate.profile}; transport: ${compensationTransportLabel(estimate.transport)}.`,
      `Assumptions: ${assumptions}.`,
      "Excludes ACK traffic, runtime packet behavior, retransmissions, and unmodeled link or tunnel encapsulation.",
    ].join(" "),
  };
}
