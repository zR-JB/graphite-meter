import {
  combineCompensationEstimates,
  estimateCompensation,
  type CompensationEstimate,
} from "../compensation";
import type { FlowDirection, TransportKind } from "../runner/contract";
import {
  MIN_EVIDENCE_MS,
  type AggregateWindow,
  type ServerMeasurementSummary,
} from "../runner/measure";

/** Estimate each component of the headline window using that participant's own path. */
export function headlineWire(
  window: AggregateWindow | null | undefined,
  dir: FlowDirection,
  path: (serverId: string) => ServerMeasurementSummary["throughput"] | null,
): CompensationEstimate | null {
  const components = window?.[dir];
  if (
    !components?.length ||
    components.some((component) => component.durationMs < MIN_EVIDENCE_MS)
  )
    return null;
  const estimates: CompensationEstimate[] = [];
  for (const component of components) {
    const evidence = path(component.serverId);
    if (
      !evidence?.clientIpVersion ||
      (evidence.transport === "fetch-stream" && !evidence.browserProtocol)
    )
      return null;
    estimates.push(
      estimateCompensation(
        component.bytesPerSec,
        evidence.browserProtocol,
        evidence.origin.startsWith("https://"),
        evidence.clientIpVersion,
        evidence.transport as TransportKind,
      ),
    );
  }
  return combineCompensationEstimates(estimates);
}
