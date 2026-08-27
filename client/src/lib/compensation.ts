// Counts protocol bytes only, excluding runtime behavior and reverse traffic.
import type {
  CompensationTransport,
  ConnectionProfile,
  OverheadCompensationConfig,
  ThroughputResult,
} from "./runner/contract";
import {
  compensationTransportFromProtocol,
  compensationTransportLabel,
  normalizeHttpProtocol,
} from "./runner/protocol";

type CompensationConfidence = "high" | "medium" | "low";
type CompensationPhase = "download" | "upload";

interface CompensationFactor {
  key:
    | "application-framing"
    | "tls-records"
    | "ethernet"
    | "ip"
    | "transport"
    | "encapsulation";
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
  profile: ConnectionProfile;
  transport: CompensationTransport;
  transportSource: "detected" | "override" | "fallback";
  mtuBytes: number;
  ipVersion: 4 | 6;
  ipVersionSource: "detected" | "override" | "fallback";
  available: boolean;
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
    profile: representative?.profile ?? "lan",
    transport: representative?.transport ?? "http1-clear",
    transportSource: representative?.transportSource ?? "fallback",
    mtuBytes: representative?.mtuBytes ?? 1_500,
    ipVersion: representative?.ipVersion ?? 4,
    ipVersionSource: representative?.ipVersionSource ?? "fallback",
    available: estimates.every((estimate) => estimate.available),
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
  return compensationTransportFromProtocol(protocol, secure);
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
  const raw = config.params;
  const params = {
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
  const ip = params.ipVersion === 6 ? WIRE.ipv6Bytes : WIRE.ipv4Bytes;
  const ethernet =
    WIRE.ethernetBytes + (params.vlanTagged ? WIRE.vlanBytes : 0);
  const tunnel = Math.max(0, params.encapsulationBytes);
  const factors: CompensationFactor[] = [];
  const ipVersionSource =
    raw.ipVersion !== "auto"
      ? "override"
      : detectedIPVersion
        ? "detected"
        : "fallback";
  const detectedTransport = normalizeHttpProtocol(detectedProtocol);
  const transportSource =
    config.transport !== "auto"
      ? "override"
      : detectedTransport && detectedTransport !== "negotiated"
        ? "detected"
        : "fallback";
  if (bytesPerSec <= 0 || config.profile === "loopback")
    return identity(
      bytesPerSec,
      transport,
      config.profile,
      params.mtuBytes,
      params.ipVersion,
      ipVersionSource,
      transportSource,
      config.profile !== "loopback",
    );

  let application = 1;
  if (transport === "http2") {
    const ratio = WIRE.http2Header / WIRE.http2Payload;
    factors.push(
      factor("application-framing", "HTTP/2 DATA frames", application * ratio),
    );
    application *= 1 + ratio;
  } else if (transport === "http3-quic") {
    const ratio = WIRE.http3Frame / WIRE.http3Payload;
    factors.push(
      factor("application-framing", "HTTP/3 DATA frames", application * ratio),
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
      const payload = Math.max(1, params.mtuBytes - ip - WIRE.udpBytes - quic);
      return (params.mtuBytes + tunnel + ethernet) / payload;
    };
    const minCid = Math.min(
      params.quicConnIdMinBytes,
      params.quicConnIdMaxBytes,
    );
    const maxCid = Math.max(
      params.quicConnIdMinBytes,
      params.quicConnIdMaxBytes,
    );
    low = link(minCid, 1);
    const centralCid = clamp(8, minCid, maxCid);
    const centralQuic =
      1 + centralCid + WIRE.quicPacketNumberTypical + WIRE.quicAeadTag;
    centralPayload = Math.max(
      1,
      params.mtuBytes - ip - WIRE.udpBytes - centralQuic,
    );
    centralTransport = WIRE.udpBytes + centralQuic;
    central = link(centralCid, WIRE.quicPacketNumberTypical);
    high = link(maxCid, 4);
  } else {
    const link = (options: number): number => {
      const payload = Math.max(
        1,
        params.mtuBytes - ip - WIRE.tcpBytes - options,
      );
      return (params.mtuBytes + tunnel + ethernet) / payload;
    };
    const minOptions = Math.max(
      0,
      Math.min(params.tcpOptionsMinBytes, params.tcpOptionsMaxBytes),
    );
    const maxOptions = Math.max(minOptions, params.tcpOptionsMaxBytes);
    centralTransport = WIRE.tcpBytes + maxOptions;
    centralPayload = Math.max(1, params.mtuBytes - ip - centralTransport);
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
  layer("ip", params.ipVersion === 6 ? "IPv6" : "IPv4", ip);
  layer(
    "transport",
    transport === "http3-quic" ? "UDP + QUIC" : "TCP + options",
    centralTransport,
  );
  if (tunnel > 0) layer("encapsulation", "Tunnel encapsulation", tunnel);

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
    profile: config.profile,
    transport,
    transportSource,
    mtuBytes: params.mtuBytes,
    ipVersion: params.ipVersion,
    ipVersionSource,
    available: true,
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
  profile: ConnectionProfile,
  mtuBytes: number,
  ipVersion: 4 | 6,
  ipVersionSource: CompensationEstimate["ipVersionSource"],
  transportSource: CompensationEstimate["transportSource"],
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
    profile,
    transport,
    transportSource,
    mtuBytes,
    ipVersion,
    ipVersionSource,
    available,
  };
}

export function compensationTooltip(estimate: CompensationEstimate): string {
  if (!estimate.available)
    return "Wire n/a\nLoopback · No physical-link estimate applies";
  const profile =
    estimate.profile === "lan"
      ? "Local Ethernet"
      : estimate.profile === "tunnel"
        ? "Tunnel"
        : estimate.profile;
  const sourceLabel = (source: CompensationEstimate["transportSource"]) =>
    source === "override"
      ? "configured"
      : source === "fallback"
        ? "assumed"
        : "detected";
  return [
    `${profile} · ${compensationTransportLabel(estimate.transport)} · ${sourceLabel(estimate.transportSource)}`,
    `IPv${estimate.ipVersion} ${sourceLabel(estimate.ipVersionSource)} · MTU ${estimate.mtuBytes} B`,
    ...estimate.factors
      .filter((factor) => factor.contributionPct > 0)
      .map(
        (factor) => `${factor.label} +${factor.contributionPct.toFixed(2)}%`,
      ),
    `Total +${((estimate.totalMultiplier - 1) * 100).toFixed(1)}%`,
  ].join("\n");
}

function clamp(value: number, min: number, max: number): number {
  return Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : min;
}
