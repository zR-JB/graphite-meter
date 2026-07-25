// Resolves the configured transfer-stream policy after protocol selection.
// Automatic H1 respects its configured browser-pool ceiling. Multiplexed
// protocols overlap finite upload POSTs. Their downloads need one stream: a
// multiplexed connection already carries it at the full connection rate.
import type {
  FlowDirection,
  ProtocolTarget,
  TransferStreamPolicy,
} from "../contract";

export const BROWSER_CONNECTION_BUDGET = 6;
export const MULTIPLEXED_UPLOAD_STREAMS = 3;
export const HTTP3_DOWNLOAD_STREAMS = 1;
const MAX_FORCED_STREAMS = 128;
/** Lanes a WebTransport session actually delivers per direction. The server
 *  clamps its server-opened download lanes here (endpoint.wtMaxStreams), and
 *  client-opened upload lanes are bounded by the peer's uni-stream credit, so a
 *  forced count above this is reported as what the transport will carry rather
 *  than what was asked for. */
export const WT_MAX_LANES = 16;

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
  if (opts.protocol === "http2" || opts.protocol === "http3") {
    if (opts.dir === "up") return MULTIPLEXED_UPLOAD_STREAMS;
    return opts.protocol === "http3" ? HTTP3_DOWNLOAD_STREAMS : 1;
  }

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

export function describeTransferStreams(
  policy: TransferStreamPolicy,
  protocol?: ProtocolTarget,
  webTransport = false,
): string {
  if (policy.mode === "forced") {
    const forced = normalizeStreamCount(policy.count);
    if (webTransport && forced > WT_MAX_LANES)
      return `Forced · ${WT_MAX_LANES} per direction (capped from ${forced} by the session)`;
    return `Forced · ${forced} per direction`;
  }
  if (webTransport) return "Automatic · 1 continuous stream per direction";
  if (protocol === "http3")
    return `Automatic · ${HTTP3_DOWNLOAD_STREAMS} download / ${MULTIPLEXED_UPLOAD_STREAMS} upload`;
  if (protocol === "http2")
    return `Automatic · 1 download / ${MULTIPLEXED_UPLOAD_STREAMS} upload`;
  return `Automatic · up to ${normalizeStreamCount(policy.count)} per direction`;
}
