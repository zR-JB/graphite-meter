import {
  combineCompensationEstimates,
  estimateCompensation,
  type CompensationEstimate,
} from "../compensation";
import type { FlowDirection, TransportKind } from "../runner/contract";
import type { MultiServerResult, TransferStage } from "./measurement";

/** Estimate each component of the chosen common window using that participant's evidence. */
export function serverWireEstimate(
  details: MultiServerResult | null,
  stage: TransferStage,
  dir: FlowDirection,
): CompensationEstimate | null {
  const interval = details?.intervals.findLast(
    (interval) => interval.stage === stage,
  );
  const components = interval?.complete ? interval.headline?.[dir] : null;
  if (
    !components?.length ||
    !details ||
    components.some((component) => component.durationMs < 800)
  )
    return null;
  const estimates: CompensationEstimate[] = [];
  for (const component of components) {
    const evidence = details.servers.find(
      (server) => server.server.id === component.serverId,
    )?.throughput;
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
