// Resolves the configured transfer-stream policy after protocol selection.
// Automatic H1 respects its configured browser-pool ceiling; H2/H3 use one.
import type {
  FlowDirection,
  ProtocolTarget,
  TransferStreamPolicy,
} from "../contract";

export const BROWSER_CONNECTION_BUDGET = 6;
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
  if (opts.policy.mode === "forced")
    return normalizeStreamCount(opts.policy.count);
  if (opts.protocol !== "http1") return 1;

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
    return "Automatic · 1 per direction";
  return `Automatic · up to ${normalizeStreamCount(policy.count)} per direction`;
}
