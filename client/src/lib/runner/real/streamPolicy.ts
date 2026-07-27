// Resolves the configured transfer-stream policy after protocol selection.
// Automatic H1 respects its configured browser-pool ceiling; a multiplexed
// protocol reads its lane counts from MULTIPLEXED_STREAMS.
import type {
  FlowDirection,
  PhaseActivity,
  ProtocolTarget,
  TransferStreamPolicy,
  TransportKind,
} from "../contract";
import { needsPings } from "./backendPure";
import { WT_MAX_LANES } from "./transports";

export const BROWSER_CONNECTION_BUDGET = 6;
const MAX_FORCED_STREAMS = 128;
/** The session lane cap, re-exported so one module answers "how many lanes". */
export { WT_MAX_LANES };

type MultiplexedProtocol = "http2" | "http3";

/** Automatic lanes per direction over a multiplexed connection, and the sole
 *  source for both the count and the text describing it. Download takes one:
 *  the connection already carries it at full rate. Upload splits by protocol —
 *  under loss h2 gains 10.1% going from 1 to 4 lanes while h3 loses 9.3% over
 *  the same range, disjoint IQRs both ways, and the loopback peaks agree.
 *  See docs/BENCHMARKS.md. */
export const MULTIPLEXED_STREAMS: Record<
  MultiplexedProtocol,
  Record<FlowDirection, number>
> = {
  http2: { down: 1, up: 4 },
  http3: { down: 1, up: 1 },
};

function multiplexed(
  protocol?: ProtocolTarget,
): protocol is MultiplexedProtocol {
  return protocol === "http2" || protocol === "http3";
}

export interface TransferStreamOptions {
  protocol: ProtocolTarget;
  policy: TransferStreamPolicy;
  transfer: readonly FlowDirection[];
  dir: FlowDirection;
  needsPing: boolean;
  /** WebTransport lanes are continuous streams with no request turnaround, so
   *  automatic mode runs one per direction. */
  webTransport?: boolean;
  totalBudget?: number;
}

export function normalizeStreamCount(count: number): number {
  return Number.isFinite(count)
    ? Math.min(MAX_FORCED_STREAMS, Math.max(1, Math.round(count)))
    : 1;
}

export function transferStreamCount(opts: TransferStreamOptions): number {
  // Forced means exact even above the browser's nominal H1 pool. Required
  // control sockets already exist when these lanes start.
  if (opts.policy.mode === "forced")
    return opts.webTransport
      ? Math.min(WT_MAX_LANES, normalizeStreamCount(opts.policy.count))
      : normalizeStreamCount(opts.policy.count);
  if (opts.webTransport) return 1;
  if (multiplexed(opts.protocol))
    return MULTIPLEXED_STREAMS[opts.protocol][opts.dir];

  const total = opts.totalBudget ?? BROWSER_CONNECTION_BUDGET;
  const controlConnections =
    (opts.needsPing ? 1 : 0) + (opts.transfer.includes("up") ? 1 : 0);
  const available = Math.max(1, total - controlConnections);
  const ceiling = normalizeStreamCount(opts.policy.count);
  if (opts.transfer.length === 1) return Math.min(available, ceiling);

  const lowerShare = Math.floor(available / 2);
  const share =
    opts.dir === opts.transfer[0]
      ? available - lowerShare
      : Math.max(1, lowerShare);
  return Math.min(share, ceiling);
}

/** The lanes a run's stages resolve to, per direction, largest first. An H1
 *  stage shares the browser budget with the control connections, so what a
 *  bidirectional stage opens is not what a download-only one does; "up to" is
 *  the largest of them. Resolved by transferStreamCount itself, so the
 *  description and the lanes cannot disagree. */
function autoStreamCeiling(
  policy: TransferStreamPolicy,
  protocol: ProtocolTarget | undefined,
  activities: readonly PhaseActivity[],
): number {
  let most = 0;
  for (const activity of activities)
    for (const dir of activity.transfer)
      most = Math.max(
        most,
        transferStreamCount({
          protocol: protocol ?? "negotiated",
          policy,
          transfer: activity.transfer,
          dir,
          needsPing: needsPings(activity),
        }),
      );
  // No stage carries bytes, so nothing resolves a count: the configured
  // ceiling is all there is to describe.
  return most || normalizeStreamCount(policy.count);
}

/** `activities` are the stages the run will execute, which is what the
 *  automatic H1 count depends on. */
export function describeTransferStreams(
  policy: TransferStreamPolicy,
  activities: readonly PhaseActivity[],
  protocol?: ProtocolTarget,
  transport?: TransportKind,
): string {
  // A datagram run opens no lanes in either direction; the transport's own
  // send queue paces it, so a lane count would describe nothing.
  if (transport === "webtransport-datagram") return "Datagram flood · no lanes";
  const webTransport = transport === "webtransport";
  if (policy.mode === "forced") {
    const forced = normalizeStreamCount(policy.count);
    if (webTransport && forced > WT_MAX_LANES)
      return `Forced · ${WT_MAX_LANES} per direction (capped from ${forced} by the session)`;
    return `Forced · ${forced} per direction`;
  }
  if (webTransport) return "Automatic · 1 continuous stream per direction";
  if (multiplexed(protocol)) {
    const lanes = MULTIPLEXED_STREAMS[protocol];
    return `Automatic · ${lanes.down} download / ${lanes.up} upload`;
  }
  return `Automatic · up to ${autoStreamCeiling(policy, protocol, activities)} per direction`;
}
