// Counts protocol bytes only, excluding runtime behavior and reverse traffic.
import type { CompensationTransport, TransportKind } from "./runner/contract";
import {
  compensationTransportFromProtocol,
  compensationTransportLabel,
  normalizeHttpProtocol,
} from "./runner/protocol";

type CompensationConfidence = "high" | "medium" | "low";

interface CompensationFactor {
  key: "application-framing" | "tls-records" | "ethernet" | "ip" | "transport";
  label: string;
  contributionPct: number;
}

export interface CompensationEstimate {
  measuredBytesPerSec: number;
  estimatedBytesPerSec: number;
  lowerBytesPerSec: number;
  upperBytesPerSec: number;
  totalMultiplier: number;
  confidence: CompensationConfidence;
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

/* This keeps bidirectional wire occupancy equal to the sum of its lanes even when their measured rates differ. */
export function combineCompensationEstimates(
  estimates: readonly CompensationEstimate[],
): CompensationEstimate {
  const active = estimates.filter(
    (estimate) => estimate.measuredBytesPerSec > 0,
  );
  const representative = active[0] ?? estimates[0];
  const measuredBytesPerSec = estimates.reduce(
    (sum, estimate) => sum + estimate.measuredBytesPerSec,
    0,
  );
  const estimatedBytesPerSec = estimates.reduce(
    (sum, estimate) => sum + estimate.estimatedBytesPerSec,
    0,
  );
  const confidenceRank: Record<CompensationConfidence, number> = {
    high: 0,
    medium: 1,
    low: 2,
  };
  const confidence = active.reduce<CompensationConfidence>(
    (lowest, estimate) =>
      confidenceRank[estimate.confidence] > confidenceRank[lowest]
        ? estimate.confidence
        : lowest,
    "high",
  );

  return {
    measuredBytesPerSec,
    estimatedBytesPerSec,
    lowerBytesPerSec: estimates.reduce(
      (sum, estimate) => sum + estimate.lowerBytesPerSec,
      0,
    ),
    upperBytesPerSec: estimates.reduce(
      (sum, estimate) => sum + estimate.upperBytesPerSec,
      0,
    ),
    totalMultiplier:
      measuredBytesPerSec > 0 ? estimatedBytesPerSec / measuredBytesPerSec : 1,
    confidence,
    factors: combineFactors(active, measuredBytesPerSec),
    transport: representative?.transport ?? "http1-clear",
    transportSource: representative?.transportSource ?? "fallback",
    framing: representative?.framing ?? null,
    mtuBytes: representative?.mtuBytes ?? 1_500,
    ipVersion: representative?.ipVersion ?? 4,
    ipVersionSource: representative?.ipVersionSource ?? "fallback",
  };
}

function combineFactors(
  estimates: readonly CompensationEstimate[],
  measuredBytesPerSec: number,
): CompensationFactor[] {
  if (!estimates.length || measuredBytesPerSec <= 0) return [];
  const template = estimates[0].factors;
  return template.map((factorTemplate) => {
    const contributionPct =
      estimates.reduce((sum, estimate) => {
        const factor = estimate.factors.find(
          (candidate) => candidate.key === factorTemplate.key,
        );
        return (
          sum + estimate.measuredBytesPerSec * (factor?.contributionPct ?? 0)
        );
      }, 0) / measuredBytesPerSec;
    return { ...factorTemplate, contributionPct };
  });
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
  quicPacketNumberTypical: 2,
  quicAeadTag: 16,
} as const;

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
  const webTransport =
    selectedTransport === "webtransport" ||
    selectedTransport === "webtransport-datagram";
  const transport = webTransport
    ? "http3-quic"
    : compensationTransportFromProtocol(detectedProtocol, secure);
  // Conservative defaults: 1500 B Ethernet, preflight IP family, standard options, no unknown VLAN/tunnel.
  const mtuBytes = 1_500;
  const ipVersion = detectedIPVersion ?? 4;
  const ip = ipVersion === 6 ? WIRE.ipv6Bytes : WIRE.ipv4Bytes;
  const ethernet = WIRE.ethernetBytes;
  const factors: CompensationFactor[] = [];
  const ipVersionSource = detectedIPVersion ? "detected" : "fallback";
  const detectedTransport = normalizeHttpProtocol(detectedProtocol);
  const transportSource =
    webTransport || (detectedTransport && detectedTransport !== "negotiated")
      ? "detected"
      : "fallback";
  const framing = webTransport
    ? selectedTransport === "webtransport-datagram"
      ? "webtransport-datagram"
      : "webtransport-stream"
    : transport === "http3-quic"
      ? "http3-data"
      : null;
  if (bytesPerSec <= 0)
    return identity(
      bytesPerSec,
      transport,
      mtuBytes,
      ipVersion,
      ipVersionSource,
      transportSource,
      framing,
    );

  let application = 1;
  if (transport === "http2") {
    const ratio = WIRE.http2Header / WIRE.http2Payload;
    factors.push(
      factor("application-framing", "HTTP/2 DATA frames", application * ratio),
    );
    application *= 1 + ratio;
  } else if (transport === "http3-quic" && framing === "http3-data") {
    const ratio = WIRE.http3Frame / WIRE.http3Payload;
    factors.push(
      factor("application-framing", "HTTP/3 DATA frames", application * ratio),
    );
    application *= 1 + ratio;
  }

  if (
    framing === "webtransport-stream" ||
    framing === "webtransport-datagram"
  ) {
    const ratio = WIRE.http3Frame / WIRE.http3Payload;
    factors.push(
      factor(
        "application-framing",
        framing === "webtransport-stream"
          ? "WebTransport QUIC stream frames"
          : "WebTransport QUIC datagrams",
        application * ratio,
      ),
    );
    application *= 1 + ratio;
  }

  if (transport === "https-tls" || transport === "http2") {
    const ratio = WIRE.tlsRecordOverhead / WIRE.tlsRecordPayload;
    factors.push(factor("tls-records", "TLS 1.3 records", application * ratio));
    application *= 1 + ratio;
  }

  let low: number;
  let central: number;
  let high: number;
  let centralPayload: number;
  let centralTransport: number;
  if (transport === "http3-quic") {
    const link = (cid: number, pn: number): number => {
      const quic = 1 + cid + pn + WIRE.quicAeadTag;
      const payload = Math.max(1, mtuBytes - ip - WIRE.udpBytes - quic);
      return (mtuBytes + ethernet) / payload;
    };
    const minCid = 0;
    const maxCid = 20;
    low = link(minCid, 1);
    const centralCid = 8;
    const centralQuic =
      1 + centralCid + WIRE.quicPacketNumberTypical + WIRE.quicAeadTag;
    centralPayload = Math.max(1, mtuBytes - ip - WIRE.udpBytes - centralQuic);
    centralTransport = WIRE.udpBytes + centralQuic;
    central = link(centralCid, WIRE.quicPacketNumberTypical);
    high = link(maxCid, 4);
  } else {
    const link = (options: number): number => {
      const payload = Math.max(1, mtuBytes - ip - WIRE.tcpBytes - options);
      return (mtuBytes + ethernet) / payload;
    };
    const minOptions = 0;
    const maxOptions = 12;
    centralTransport = WIRE.tcpBytes + maxOptions;
    centralPayload = Math.max(1, mtuBytes - ip - centralTransport);
    low = link(minOptions);
    central = link(maxOptions);
    high = central;
  }

  const layer = (
    key: CompensationFactor["key"],
    label: string,
    bytes: number,
  ) => factors.push(factor(key, label, application * (bytes / centralPayload)));
  layer("ethernet", "Ethernet", ethernet);
  layer("ip", ipVersion === 6 ? "IPv6" : "IPv4", ip);
  layer(
    "transport",
    transport === "http3-quic" ? "UDP + QUIC" : "TCP + options",
    centralTransport,
  );

  low *= application;
  central *= application;
  high *= application;
  return {
    measuredBytesPerSec: bytesPerSec,
    estimatedBytesPerSec: bytesPerSec * central,
    lowerBytesPerSec: bytesPerSec * Math.min(low, high),
    upperBytesPerSec: bytesPerSec * Math.max(low, high),
    totalMultiplier: central,
    confidence: low === high ? "high" : "medium",
    factors,
    transport,
    transportSource,
    framing,
    mtuBytes,
    ipVersion,
    ipVersionSource,
  };
}

function factor(
  key: CompensationFactor["key"],
  label: string,
  ratio: number,
): CompensationFactor {
  return { key, label, contributionPct: ratio * 100 };
}

function identity(
  bytesPerSec: number,
  transport: CompensationTransport,
  mtuBytes: number,
  ipVersion: 4 | 6,
  ipVersionSource: CompensationEstimate["ipVersionSource"],
  transportSource: CompensationEstimate["transportSource"],
  framing: CompensationEstimate["framing"],
): CompensationEstimate {
  return {
    measuredBytesPerSec: bytesPerSec,
    estimatedBytesPerSec: bytesPerSec,
    lowerBytesPerSec: bytesPerSec,
    upperBytesPerSec: bytesPerSec,
    totalMultiplier: 1,
    confidence: "high",
    factors: [],
    transport,
    transportSource,
    framing,
    mtuBytes,
    ipVersion,
    ipVersionSource,
  };
}

export function compensationTooltip(estimate: CompensationEstimate): string {
  const sourceLabel = (source: CompensationEstimate["transportSource"]) =>
    source === "fallback" ? "assumed" : "detected";
  return [
    `Local Ethernet · ${compensationTransportLabel(estimate.transport)} · assumed`,
    `Transport ${sourceLabel(estimate.transportSource)}`,
    `IPv${estimate.ipVersion} ${sourceLabel(estimate.ipVersionSource)} · MTU ${estimate.mtuBytes} B`,
    ...estimate.factors
      .filter((factor) => factor.contributionPct > 0)
      .map(
        (factor) => `${factor.label} +${factor.contributionPct.toFixed(2)}%`,
      ),
    `Total +${((estimate.totalMultiplier - 1) * 100).toFixed(1)}%`,
  ].join("\n");
}
