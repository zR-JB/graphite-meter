// Converts application goodput into forward-direction physical link occupancy.
// Only protocol bytes are added; runtime behavior and reverse traffic are not.
import type {
  CompensationTransport,
  ConnectionProfile,
  OverheadCompensationConfig,
  ThroughputResult,
} from "./runner/contract";

export type CompensationConfidence = "high" | "medium" | "low";
export type CompensationPhase = "download" | "upload";

export interface CompensationFactor {
  key:
    "application-framing" | "tls-records" | "network-framing" | "encapsulation";
  label: string;
  multiplier: number;
  ratio: number;
  confidence: CompensationConfidence;
  detail: string;
}

export interface CompensationEstimate {
  measuredBytesPerSec: number;
  estimatedBytesPerSec: number;
  lowerBytesPerSec: number;
  upperBytesPerSec: number;
  totalMultiplier: number;
  confidence: CompensationConfidence;
  factors: CompensationFactor[];
  transport: CompensationTransport;
  assumptions: string[];
  available: boolean;
}

const C = {
  ethernetBytes: 38, // 14 MAC + 4 FCS + 8 preamble/SFD + 12 inter-frame gap
  vlanBytes: 4,
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

export function transportFromProtocol(
  protocol: string | undefined,
  secure = typeof location !== "undefined" && location.protocol === "https:",
): CompensationTransport {
  const p = protocol?.toLowerCase() ?? "";
  if (p === "h3" || p.startsWith("h3-")) return "http3-quic";
  if (p === "h2" || p === "h2c") return "http2";
  return secure ? "https-tls" : "http1-clear";
}

export function applyConnectionProfile(
  profile: ConnectionProfile,
): Pick<OverheadCompensationConfig, "profile" | "params"> {
  const tunnel = profile === "tunnel";
  const loopback = profile === "loopback";
  return {
    profile,
    params: {
      mtuBytes: loopback ? 65_536 : tunnel ? 1_420 : 1_500,
      ipVersion: 4,
      vlanTagged: false,
      tcpOptionsMinBytes: 0,
      tcpOptionsMaxBytes: 12,
      encapsulationBytes: tunnel ? 60 : 0,
      quicConnIdMinBytes: 0,
      quicConnIdMaxBytes: 20,
    },
  };
}

export function estimateResultCompensation(
  result: ThroughputResult | null,
  phase: CompensationPhase,
  config: OverheadCompensationConfig,
  detectedProtocol?: string,
  detectedSecure?: boolean,
  detectedIPVersion?: 4 | 6,
): CompensationEstimate {
  return estimateLiveCompensation(
    result?.meanBytesPerSec ?? 0,
    config,
    phase,
    detectedProtocol,
    detectedSecure,
    detectedIPVersion,
  );
}

export function estimateLiveCompensation(
  bytesPerSec: number,
  config: OverheadCompensationConfig,
  _phase: CompensationPhase,
  detectedProtocol?: string,
  detectedSecure?: boolean,
  detectedIPVersion?: 4 | 6,
): CompensationEstimate {
  const secure =
    detectedSecure ??
    (typeof location !== "undefined" && location.protocol === "https:");
  const transport =
    config.transport === "auto"
      ? transportFromProtocol(detectedProtocol, secure)
      : config.transport;
  if (bytesPerSec <= 0 || config.profile === "loopback")
    return identity(bytesPerSec, transport, config.profile !== "loopback");

  const raw = config.params;
  const p = {
    ...raw,
    ipVersion:
      raw.ipVersion === "auto" ? (detectedIPVersion ?? 4) : raw.ipVersion,
    mtuBytes: clamp(raw.mtuBytes, 576, 65_536),
    tcpOptionsMinBytes: clamp(raw.tcpOptionsMinBytes, 0, 40),
    tcpOptionsMaxBytes: clamp(raw.tcpOptionsMaxBytes, 0, 40),
    encapsulationBytes: clamp(raw.encapsulationBytes, 0, 256),
    quicConnIdMinBytes: clamp(raw.quicConnIdMinBytes, 0, 20),
    quicConnIdMaxBytes: clamp(raw.quicConnIdMaxBytes, 0, 20),
  };
  const ip = p.ipVersion === 6 ? C.ipv6Bytes : C.ipv4Bytes;
  const ethernet = C.ethernetBytes + (p.vlanTagged ? C.vlanBytes : 0);
  const tunnel = Math.max(0, p.encapsulationBytes);
  const factors: CompensationFactor[] = [];

  let application = 1;
  if (transport === "http2") {
    application *= (C.http2Payload + C.http2Header) / C.http2Payload;
    factors.push(
      factor(
        "application-framing",
        "HTTP/2 DATA frames",
        C.http2Header / C.http2Payload,
        "high",
        "9 B per 16 KiB DATA frame",
      ),
    );
  } else if (transport === "http3-quic") {
    application *= (C.http3Payload + C.http3Frame) / C.http3Payload;
    factors.push(
      factor(
        "application-framing",
        "HTTP/3 DATA frames",
        C.http3Frame / C.http3Payload,
        "medium",
        "variable-length frame header",
      ),
    );
  }

  if (transport === "https-tls" || transport === "http2") {
    const ratio = C.tlsRecordOverhead / C.tlsRecordPayload;
    application *= 1 + ratio;
    factors.push(
      factor(
        "tls-records",
        "TLS 1.3 records",
        ratio,
        "medium",
        "22 B per 16 KiB record (AES-GCM/ChaCha20-Poly1305)",
      ),
    );
  }

  let low: number;
  let central: number;
  let high: number;
  if (transport === "http3-quic") {
    const link = (cid: number, pn: number): number => {
      const quic = 1 + cid + pn + C.quicAeadTag;
      const payload = Math.max(1, p.mtuBytes - ip - C.udpBytes - quic);
      return (p.mtuBytes + tunnel + ethernet) / payload;
    };
    const minCid = Math.min(p.quicConnIdMinBytes, p.quicConnIdMaxBytes);
    const maxCid = Math.max(p.quicConnIdMinBytes, p.quicConnIdMaxBytes);
    low = link(minCid, 1);
    central = link(clamp(8, minCid, maxCid), C.quicPacketNumberTypical);
    high = link(maxCid, 4);
  } else {
    const link = (options: number): number => {
      const payload = Math.max(1, p.mtuBytes - ip - C.tcpBytes - options);
      return (p.mtuBytes + tunnel + ethernet) / payload;
    };
    const minOptions = Math.max(
      0,
      Math.min(p.tcpOptionsMinBytes, p.tcpOptionsMaxBytes),
    );
    const maxOptions = Math.max(minOptions, p.tcpOptionsMaxBytes);
    low = link(minOptions);
    central = link(maxOptions);
    high = central;
  }

  const networkRatio = central - 1;
  factors.push(
    factor(
      "network-framing",
      transport === "http3-quic"
        ? "Ethernet / IP / UDP / QUIC"
        : "Ethernet / IP / TCP",
      networkRatio,
      "medium",
      `${p.mtuBytes} B MTU; ${p.ipVersion === 6 ? "IPv6" : "IPv4"}`,
    ),
  );
  if (tunnel > 0)
    factors.push(
      factor(
        "encapsulation",
        "Tunnel encapsulation",
        tunnel / p.mtuBytes,
        "medium",
        `${tunnel} B outer overhead per packet`,
      ),
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
    assumptions: [
      `${p.ipVersion === 6 ? "IPv6" : "IPv4"}, ${p.mtuBytes} B MTU`,
      transport === "http3-quic"
        ? `${Math.min(p.quicConnIdMinBytes, p.quicConnIdMaxBytes)}–${Math.max(p.quicConnIdMinBytes, p.quicConnIdMaxBytes)} B QUIC connection ID`
        : `${Math.min(p.tcpOptionsMinBytes, p.tcpOptionsMaxBytes)}–${Math.max(p.tcpOptionsMinBytes, p.tcpOptionsMaxBytes)} B TCP options`,
      ...(tunnel ? [`${tunnel} B tunnel encapsulation`] : []),
    ],
    available: true,
  };
}

function factor(
  key: CompensationFactor["key"],
  label: string,
  ratio: number,
  confidence: CompensationConfidence,
  detail: string,
): CompensationFactor {
  return { key, label, ratio, multiplier: 1 + ratio, confidence, detail };
}

function identity(
  bytesPerSec: number,
  transport: CompensationTransport,
  available = true,
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
    assumptions: [],
    available,
  };
}

function clamp(value: number, min: number, max: number): number {
  return Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : min;
}
