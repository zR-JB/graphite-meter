// Counts protocol bytes only, excluding runtime behavior and reverse traffic.
import type { CompensationTransport, TransportKind } from "./runner/contract";
import { normalizeHttpProtocol } from "./runner/paths";

type CompensationConfidence = "high" | "medium" | "low";
type FactorKey =
  "application-framing" | "tls-records" | "ethernet" | "ip" | "transport";

interface CompensationFactor {
  key: FactorKey;
  label: string;
  contributionPct: number;
}

export interface CompensationBreakdown {
  componentCount?: number;
  factors: CompensationFactor[];
  /** Provenance for the exact assumptions that produced this estimate. */
  transport: CompensationTransport;
  transportSource: "detected" | "fallback";
  framing:
    "http3-data" | "webtransport-stream" | "webtransport-datagram" | null;
  mtuBytes: number;
  ipVersion: 4 | 6;
  ipVersionSource: "detected" | "fallback";
}

export interface CompensationEstimate extends CompensationBreakdown {
  measuredBytesPerSec: number;
  estimatedBytesPerSec: number;
  lowerBytesPerSec: number;
  upperBytesPerSec: number;
  totalMultiplier: number;
  confidence: CompensationConfidence;
}

const CONFIDENCE: CompensationConfidence[] = ["high", "medium", "low"];
const MIXED_LABELS: Record<FactorKey, string> = {
  "application-framing": "Application framing",
  "tls-records": "TLS records",
  ethernet: "Ethernet",
  ip: "IP headers",
  transport: "Transport headers",
};

/* Bidirectional wire occupancy is the sum of its lanes even when their measured rates differ. */
export function combineCompensationEstimates(
  estimates: readonly CompensationEstimate[],
): CompensationEstimate {
  const active = estimates.filter(
    (estimate) => estimate.measuredBytesPerSec > 0,
  );
  const first = active[0] ?? estimates[0];
  const sum = (pick: (estimate: CompensationEstimate) => number) =>
    estimates.reduce((total, estimate) => total + pick(estimate), 0);
  const measuredBytesPerSec = sum((e) => e.measuredBytesPerSec);
  const estimatedBytesPerSec = sum((e) => e.estimatedBytesPerSec);
  const keys = [
    ...new Set(active.flatMap((e) => e.factors.map((factor) => factor.key))),
  ];
  const factors =
    measuredBytesPerSec > 0
      ? keys.map((key) => {
          const matching = active.flatMap((e) =>
            e.factors.filter((factor) => factor.key === key),
          );
          const labels = new Set(matching.map((factor) => factor.label));
          const weighted = active.reduce((total, e) => {
            const factor = e.factors.find((candidate) => candidate.key === key);
            return (
              total + e.measuredBytesPerSec * (factor?.contributionPct ?? 0)
            );
          }, 0);
          return {
            key,
            label: labels.size === 1 ? [...labels][0] : MIXED_LABELS[key],
            contributionPct: weighted / measuredBytesPerSec,
          };
        })
      : [];
  return {
    componentCount: sum((e) => e.componentCount ?? 1),
    measuredBytesPerSec,
    estimatedBytesPerSec,
    lowerBytesPerSec: sum((e) => e.lowerBytesPerSec),
    upperBytesPerSec: sum((e) => e.upperBytesPerSec),
    totalMultiplier:
      measuredBytesPerSec > 0 ? estimatedBytesPerSec / measuredBytesPerSec : 1,
    confidence:
      CONFIDENCE[
        Math.max(0, ...active.map((e) => CONFIDENCE.indexOf(e.confidence)))
      ],
    factors,
    transport: first?.transport ?? "http1-clear",
    transportSource: first?.transportSource ?? "fallback",
    framing: first?.framing ?? null,
    mtuBytes: first?.mtuBytes ?? 1_500,
    ipVersion: first?.ipVersion ?? 4,
    ipVersionSource: first?.ipVersionSource ?? "fallback",
  };
}

const WIRE = {
  ethernetBytes: 38, // 14 MAC + 4 FCS + 8 preamble/SFD + 12 inter-frame gap
  ipv4Bytes: 20,
  ipv6Bytes: 40,
  tcpBytes: 20,
  udpBytes: 8,
  tlsRecordPayload: 16_384,
  tlsRecordOverhead: 22, // 5 record + 1 inner type + 16-byte AEAD tag
  http2Payload: 16_384,
  http2Header: 9,
  http3Payload: 16_384,
  http3Frame: 5, // one-byte type plus four-byte length at a 16 KiB DATA frame
  quicAeadTag: 16,
} as const;
const FRAMING_LABEL = {
  "http3-data": "HTTP/3 DATA frames",
  "webtransport-stream": "WebTransport QUIC stream frames",
  "webtransport-datagram": "WebTransport QUIC datagrams",
} as const;
/* QUIC short headers as [connection ID, packet number] bytes: least, typical, most. */
const QUIC_HEADERS = [
  [0, 1],
  [8, 2],
  [20, 4],
];
/* TCP option bytes: none, then timestamps as both the typical and the most. */
const TCP_OPTIONS = [0, 12, 12];

export function estimateCompensation(
  bytesPerSec: number,
  detectedProtocol?: string,
  detectedSecure?: boolean,
  detectedIPVersion?: 4 | 6,
  selectedTransport?: TransportKind,
): CompensationEstimate {
  const secure =
    detectedSecure ??
    (typeof location !== "undefined" && location.protocol === "https:");
  const detected = normalizeHttpProtocol(detectedProtocol);
  const framing =
    selectedTransport === "webtransport-datagram"
      ? "webtransport-datagram"
      : selectedTransport === "webtransport"
        ? "webtransport-stream"
        : detected === "http3"
          ? "http3-data"
          : null;
  const transport: CompensationTransport = framing
    ? "http3-quic"
    : detected === "http2"
      ? "http2"
      : secure
        ? "https-tls"
        : "http1-clear";
  // Conservative defaults: 1500 B Ethernet, preflight IP family, standard options, no unknown VLAN/tunnel.
  const mtuBytes = 1_500;
  const ipVersion = detectedIPVersion ?? 4;
  const estimate: CompensationEstimate = {
    measuredBytesPerSec: bytesPerSec,
    estimatedBytesPerSec: bytesPerSec,
    lowerBytesPerSec: bytesPerSec,
    upperBytesPerSec: bytesPerSec,
    totalMultiplier: 1,
    confidence: "high",
    factors: [],
    transport,
    transportSource:
      selectedTransport?.startsWith("webtransport") ||
      (detected && detected !== "negotiated")
        ? "detected"
        : "fallback",
    framing,
    mtuBytes,
    ipVersion,
    ipVersionSource: detectedIPVersion ? "detected" : "fallback",
  };
  if (bytesPerSec <= 0) return estimate;

  const { factors } = estimate;
  let application = 1;
  const wrap = (key: FactorKey, label: string, ratio: number) => {
    factors.push(factor(key, label, application * ratio));
    application *= 1 + ratio;
  };
  if (transport === "http2")
    wrap(
      "application-framing",
      "HTTP/2 DATA frames",
      WIRE.http2Header / WIRE.http2Payload,
    );
  if (framing)
    wrap(
      "application-framing",
      FRAMING_LABEL[framing],
      WIRE.http3Frame / WIRE.http3Payload,
    );
  if (transport === "https-tls" || transport === "http2")
    wrap(
      "tls-records",
      "TLS 1.3 records",
      WIRE.tlsRecordOverhead / WIRE.tlsRecordPayload,
    );

  const ip = ipVersion === 6 ? WIRE.ipv6Bytes : WIRE.ipv4Bytes;
  const headers = framing
    ? QUIC_HEADERS.map(
        ([cid, pn]) => WIRE.udpBytes + 1 + cid + pn + WIRE.quicAeadTag,
      )
    : TCP_OPTIONS.map((options) => WIRE.tcpBytes + options);
  const payload = (header: number) => Math.max(1, mtuBytes - ip - header);
  const [low, central, high] = headers.map(
    (header) =>
      (application * (mtuBytes + WIRE.ethernetBytes)) / payload(header),
  );
  const layer = (key: FactorKey, label: string, bytes: number) =>
    factors.push(
      factor(key, label, application * (bytes / payload(headers[1]))),
    );
  layer("ethernet", "Ethernet", WIRE.ethernetBytes);
  layer("ip", ipVersion === 6 ? "IPv6" : "IPv4", ip);
  layer("transport", framing ? "UDP + QUIC" : "TCP + options", headers[1]);
  return {
    ...estimate,
    estimatedBytesPerSec: bytesPerSec * central,
    lowerBytesPerSec: bytesPerSec * Math.min(low, high),
    upperBytesPerSec: bytesPerSec * Math.max(low, high),
    totalMultiplier: central,
    confidence: low === high ? "high" : "medium",
  };
}

function factor(
  key: FactorKey,
  label: string,
  ratio: number,
): CompensationFactor {
  return { key, label, contributionPct: ratio * 100 };
}

export function compensationTooltip(estimate: CompensationBreakdown): string {
  return [
    ...estimate.factors
      .filter((factor) => factor.contributionPct > 0)
      .map(
        (factor) => `${factor.label} +${factor.contributionPct.toFixed(2)}%`,
      ),
    `MTU ${estimate.mtuBytes.toLocaleString("en-US")} B assumed`,
  ].join("\n");
}
