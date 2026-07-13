// Resolves the configured transfer-stream policy after protocol selection.
// Automatic H1 respects its configured browser-pool ceiling. Multiplexed
// protocols need one download request but overlapping finite upload POSTs.
import type {
  FlowDirection,
  ProtocolTarget,
  TransferStreamPolicy,
} from "../contract";

export const BROWSER_CONNECTION_BUDGET = 6;
export const MULTIPLEXED_UPLOAD_STREAMS = 3;
export const MAX_FORCED_STREAMS = 128;

export interface TransferStreamOptions {
  protocol: ProtocolTarget;
  policy: TransferStreamPolicy;
  transfer: readonly FlowDirection[];
  dir: FlowDirection;
  needsPing: boolean;
  totalBudget?: number;
}

export function normalizeStreamCount(count: number): number {
  return Number.isFinite(count)
    ? Math.min(MAX_FORCED_STREAMS, Math.max(1, Math.round(count)))
    : 1;
}

export function transferStreamCount(opts: TransferStreamOptions): number {
  // Forced means exact even above the browser's nominal H1 pool. Required
  // control sockets are established before these lanes are started.
  if (opts.policy.mode === "forced")
    return normalizeStreamCount(opts.policy.count);
  if (opts.protocol !== "http1")
    return opts.dir === "up" ? MULTIPLEXED_UPLOAD_STREAMS : 1;

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
): string {
  if (policy.mode === "forced")
    return `Forced · ${normalizeStreamCount(policy.count)} per direction`;
  if (protocol === "http2" || protocol === "http3")
    return `Automatic · 1 download / ${MULTIPLEXED_UPLOAD_STREAMS} upload`;
  return `Automatic · up to ${normalizeStreamCount(policy.count)} per direction`;
}
