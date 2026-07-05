/* ============================================================
 * The Graphite Meter — Overhead Compensation Engine
 * ------------------------------------------------------------
 * Pure TypeScript, no Svelte. Estimates the TRUE WIRE-RATE that
 * a measured browser throughput implies, by accounting for the
 * bytes the browser never "sees" but the access link still
 * carries: Ethernet/IP/L4 framing, TLS record expansion,
 * application framing (HTTP/2, HTTP/3+QUIC, WebSocket masks),
 * reverse-path control (ACKs), loss/retransmission tax, the
 * ramp toward steady-state plateau, and browser-runtime jitter.
 *
 * A 7-factor estimator with two design choices:
 *   1. CANONICAL UNIT IS bytes/sec (browser-native), not bits/sec — the
 *      store stays bytes-canonical, so every estimate is bytes/sec in,
 *      bytes/sec out. Multipliers are unitless, so the byte-accounting
 *      math is identical; only the carried value differs.
 *   2. NO server-reported metadata object — the transfer rides
 *      TCP/TLS over HTTP (fetch streams) and
 *      the endpoint is assumed TLS (port 443). Tunable byte counts
 *      live in `config.compensation.params`.
 *
 * Every coefficient is a named, commented entry in COMPENSATION_DEFAULTS
 * below — the factor functions contain NO bare magic numbers.
 *
 * Hot-path note: `estimateResultCompensation` is the full,
 * sample-array-walking estimate (run once on `complete`).
 * `estimateLiveCompensation` is the O(1) live path — it applies
 * ONLY the protocol/config multipliers and NEVER iterates or
 * recomputes per-sample, so it is safe on the live render cadence.
 * ============================================================ */

import type {
  CompensationTransport,
  ConnectionProfile,
  OverheadCompensationConfig,
  ThroughputResult,
} from "./runner/contract";

/* ---------- Public types ---------- */

export type CompensationConfidence = "high" | "medium" | "low";
export type CompensationPhase = "download" | "upload";

/** A single overhead category's contribution to the wire-rate estimate. */
export interface CompensationFactor {
  key:
    | "ethernet-framing"
    | "encapsulation"
    | "tls-records"
    | "application-framing"
    | "reverse-path-control"
    | "loss-retransmission"
    | "receiver-bias"
    | "steady-state-ramp"
    | "browser-runtime";
  label: string;
  /** ≥ 1.0 multiplier this factor contributes to the total. */
  multiplier: number;
  /** `multiplier − 1`, i.e. the fractional lift (0.027 = +2.7%). */
  ratio: number;
  confidence: CompensationConfidence;
  /** Human-readable explanation of where the bytes come from. */
  detail: string;
}

/** Aggregate wire-rate estimate for one phase. */
export interface CompensationEstimate {
  measuredBytesPerSec: number;
  estimatedBytesPerSec: number;
  /** Product of every active factor's multiplier (1.0 when disabled). */
  totalMultiplier: number;
  confidence: CompensationConfidence;
  factors: CompensationFactor[];
}

/* ============================================================
 * COMPENSATION_DEFAULTS — every coefficient, named and
 * commented. User-tunable protocol params live in
 * `config.compensation.params`; the fixed protocol constants
 * (header sizes etc.) and heuristic weights live here.
 * ============================================================ */

export const COMPENSATION_DEFAULTS = {
  /* ---- Fixed Ethernet / link-layer accounting (RFC-defined) ---- */
  ethernetPreambleAndGapBytes: 20, // 7B preamble + 1B SFD + 12B inter-packet gap
  ethernetMacAndFcsBytes: 18, // 6B dst + 6B src MAC + 2B EtherType + 4B FCS
  vlanTagBytes: 4, // one 802.1Q tag adds 4B per frame
  /* ---- Fixed IP / transport header sizes ---- */
  ipv4HeaderBytes: 20, // IPv4 base header (no options)
  ipv6HeaderBytes: 40, // IPv6 fixed header
  tcpBaseHeaderBytes: 20, // TCP header without options
  udpHeaderBytes: 8, // UDP header (carries QUIC)
  /* ---- Fixed TLS record accounting ---- */
  tlsRecordHeaderBytes: 5, // TLS record content-type + version + length
  tlsInnerContentTypeBytes: 1, // TLS 1.3 inner content-type byte
  /* ---- Fixed application-framing accounting ---- */
  http2FrameHeaderBytes: 9, // HTTP/2 frame header (length+type+flags+streamID)
  http3FrameOverheadBytes: 2, // HTTP/3 DATA frame header estimate (varint type+len)
  quicShortHeaderBaseBytes: 2, // QUIC short-header flags + packet-number bytes
  /* ---- QUIC payload clamp ---- */
  quicMaxPayloadBytes: 1200, // conservative initial QUIC max datagram payload
  /* ---- Ethernet framing guard ---- */
  minTransportPayloadBytes: 256, // floor so a tiny MTU can't explode the ratio
  /* ---- Reverse-path (ACK) heuristic ---- */
  ackBytesPerTwoPackets: 84, // one ~84B (incl. framing) ACK per two data packets
  ackPacketsPerData: 2, // ACK ratio: one ACK acknowledges two data packets
  quicReversePathRatio: 0.015, // flat 1.5% bidirectional QUIC control estimate
  /* ---- Steady-state ramp heuristic ---- */
  steadyStatePlateauPercentile: 0.65, // average the top 35% (≥ p65) as the plateau
  steadyStateMinLift: 0.005, // ignore lifts under 0.5% (noise floor)
  /* ---- Browser-runtime tax heuristic ---- */
  runtimeVarianceWeight: 0.08, // map coefficient-of-variation → tax via this weight
  runtimeMinTax: 0.002, // ignore taxes under 0.2% (noise floor)
  /* ---- Receiver-bias (download) measurement correction ---- */
  // The browser RECEIVER is CPU-bound and times over wall-dt, so its counted
  // rate sits below true delivery, and the gap GROWS with rate. The Go receiver
  // (upload) is active-timed and clean, so this applies to DOWNLOAD only. A
  // saturating (Michaelis–Menten) curve: ~0 at low rate, → ceil at multi-gigabit.
  receiverBiasCeilRatio: 0.045, // asymptotic lift ceiling (≈ +4.5% as rate → ∞)
  receiverBiasHalfRateBytes: 1.5e8, // rate (≈1.2 Gbit/s) at which lift = ½ ceil
  receiverBiasMinLift: 0.001, // ignore lifts under 0.1% (noise floor)
  /* ---- Confidence noise floor for factor inclusion ---- */
  pushFactorEpsilon: 1, // only factors with multiplier > 1 are recorded
} as const;

/* ============================================================
 * PUBLIC ENTRY POINTS
 * ============================================================ */

/**
 * Full end-of-run estimate for one phase. May use sample-derived
 * factors (steady-state ramp, browser-runtime variance) from the
 * result-adjacent throughput stats. Call once on `complete`, never
 * per sample.
 *
 * NOTE: `ThroughputResult` (this repo's contract) does NOT carry the
 * raw sample array, only aggregate stats (mean/peak/stability). We
 * reconstruct the two sample-derived factors from those aggregates:
 *   - steady-state ramp ← peak vs mean (the plateau-vs-average proxy)
 *   - browser-runtime   ← stabilityPct → coefficient-of-variation
 * This keeps the math runner-agnostic and avoids threading the full
 * sample buffer through the result type.
 */
export function estimateResultCompensation(
  result: ThroughputResult | null,
  phase: CompensationPhase,
  config: OverheadCompensationConfig,
): CompensationEstimate {
  const measuredBytesPerSec = result?.meanBytesPerSec ?? 0;

  // Disabled, no data, or no factors → identity (1.0) multiplier.
  if (!config.enabled || measuredBytesPerSec <= 0 || result == null) {
    return identityEstimate(measuredBytesPerSec);
  }

  const factors: CompensationFactor[] = [];

  // Protocol/config-based factors (same set the live path uses). `phase` selects
  // direction-asymmetric factors (the download-only receiver-bias correction);
  // the wire-framing factors are direction-symmetric.
  collectProtocolFactors(factors, config, phase, measuredBytesPerSec);

  // Loss/retransmission — driven by the under-load packet loss the runner now
  // surfaces on ThroughputResult (loaded-ping loss). Capped by maxLossRatio;
  // contributes nothing when no loss was observed.
  if (config.factors.lossRetransmission) {
    pushFactor(
      factors,
      lossRetransmissionFactor(result.packetLossPct / 100, config),
    );
  }

  // Sample-derived factors, reconstructed from aggregate stats.
  if (config.factors.steadyStateRamp) {
    pushFactor(factors, steadyStateRampFactor(result));
  }
  if (config.factors.browserRuntime) {
    pushFactor(factors, browserRuntimeFactor(result));
  }

  return finalize(measuredBytesPerSec, factors);
}

/**
 * CHEAP, O(1) live estimate. Applies ONLY the protocol/config
 * multipliers (ethernet framing, TLS records, application framing,
 * reverse-path control) to a single instantaneous bytesPerSec value. Skips
 * every sample-array-heavy factor (steady-state ramp, browser-runtime)
 * so it is safe to call on the live render cadence — it never touches
 * a sample buffer.
 */
export function estimateLiveCompensation(
  bytesPerSec: number,
  config: OverheadCompensationConfig,
  phase: CompensationPhase,
): CompensationEstimate {
  if (!config.enabled || bytesPerSec <= 0) return identityEstimate(bytesPerSec);

  const factors: CompensationFactor[] = [];
  collectProtocolFactors(factors, config, phase, bytesPerSec);
  return finalize(bytesPerSec, factors);
}

/**
 * Sensible factor + param defaults for a (connection profile, transport) pair.
 * The UI calls this when the user changes either selector and merges the result
 * over `config.compensation.{factors,params}`; the raw knobs stay editable in the
 * Advanced disclosure. Pure — no side effects. The wire-framing factor functions
 * also self-gate on `transport`, so a stale toggle can never produce a wrong
 * factor (e.g. TLS on cleartext HTTP/1.1); this just sets the obvious defaults.
 */
export function applyConnectionProfile(
  profile: ConnectionProfile,
  transport: CompensationTransport,
): {
  factors: Pick<
    OverheadCompensationConfig["factors"],
    | "ethernetFraming"
    | "encapsulation"
    | "tlsRecords"
    | "applicationFraming"
    | "reversePathControl"
    | "receiverBias"
  >;
  params: Pick<
    OverheadCompensationConfig["params"],
    "mtuBytes" | "ipVersion" | "encapsulationBytes" | "tcpOptionsBytes"
  >;
} {
  const tls = transport === "https-tls" || transport === "http2";
  const appFraming = transport === "http2" || transport === "http3-quic";
  const quic = transport === "http3-quic";
  const loopback = profile === "loopback";
  const tunnel = profile === "tunnel";
  // Presets default to IPv4 (the common case); flip ipVersion + encapsulationBytes
  // in the Advanced disclosure for an IPv6 path or a non-WireGuard tunnel.
  const ipVersion = 4 as const;
  return {
    factors: {
      // No link layer on loopback; real NICs and tunnels carry Ethernet/IP/L4.
      ethernetFraming: !loopback,
      encapsulation: tunnel,
      tlsRecords: tls,
      applicationFraming: appFraming,
      reversePathControl: !loopback,
      // Browser receive-cost bias matters on real NICs/tunnels; loopback receive
      // is cheap (download there reads at or above upload), so leave it off.
      receiverBias: !loopback,
    },
    params: {
      mtuBytes: loopback ? 65536 : tunnel ? 1420 : 1500,
      ipVersion,
      encapsulationBytes: tunnel ? 60 : 0, // WireGuard IPv4 outer header
      tcpOptionsBytes: quic ? 0 : 12, // QUIC rides UDP — no TCP options
    },
  };
}

/* ============================================================
 * FACTOR ASSEMBLY
 * ============================================================ */

/** The protocol/config-based factors shared by live + result paths. `rate` is
 *  the bytes/sec the factors are applied to (instantaneous live, or phase mean),
 *  used by the rate-aware receiver-bias correction. */
function collectProtocolFactors(
  factors: CompensationFactor[],
  config: OverheadCompensationConfig,
  phase: CompensationPhase,
  rate: number,
): void {
  if (config.factors.applicationFraming) {
    pushFactor(factors, applicationFramingFactor(config));
  }
  if (config.factors.tlsRecords) {
    pushFactor(factors, tlsRecordFactor(config));
  }
  if (config.factors.ethernetFraming) {
    pushFactor(factors, ethernetFramingFactor(config));
  }
  if (config.factors.encapsulation) {
    pushFactor(factors, encapsulationFactor(config));
  }
  if (config.factors.reversePathControl) {
    pushFactor(factors, reversePathControlFactor(config));
  }
  if (config.factors.receiverBias) {
    pushFactor(factors, receiverBiasFactor(phase, rate, config));
  }
}

function finalize(
  measuredBytesPerSec: number,
  factors: CompensationFactor[],
): CompensationEstimate {
  const totalMultiplier = factors.reduce((p, f) => p * f.multiplier, 1);
  return {
    measuredBytesPerSec,
    estimatedBytesPerSec: measuredBytesPerSec * totalMultiplier,
    totalMultiplier,
    confidence: combinedConfidence(factors),
    factors,
  };
}

function identityEstimate(bytesPerSec: number): CompensationEstimate {
  return {
    measuredBytesPerSec: bytesPerSec,
    estimatedBytesPerSec: bytesPerSec,
    totalMultiplier: 1,
    confidence: "high",
    factors: [],
  };
}

/** Only record factors that actually add bytes (multiplier > 1). */
function pushFactor(
  factors: CompensationFactor[],
  factor: CompensationFactor | null,
): void {
  if (!factor || factor.multiplier <= COMPENSATION_DEFAULTS.pushFactorEpsilon)
    return;
  factors.push(factor);
}

/* ============================================================
 * INDIVIDUAL FACTORS
 * Each returns a multiplier (payload+overhead)/payload and a
 * confidence reflecting how deterministic the accounting is:
 *   high   = exact protocol byte accounting
 *   medium = byte accounting with an assumed protocol/param
 *   low    = heuristic (control traffic, ramp, runtime jitter)
 * ============================================================ */

/** Whether the transfer rides QUIC (HTTP/3) vs TCP. Drives the UDP-vs-TCP header
 *  in ethernet framing and the QUIC-vs-HTTP/2 application-framing branch. */
function isQuic(config: OverheadCompensationConfig): boolean {
  return config.transport === "http3-quic";
}

/** Whether the transfer is wrapped in TLS records (HTTP/1.1-over-TLS or HTTP/2).
 *  QUIC carries its own packet protection (modeled in application framing), and
 *  cleartext HTTP/1.1 has none. */
function usesTls(config: OverheadCompensationConfig): boolean {
  return config.transport === "https-tls" || config.transport === "http2";
}

/**
 * Application framing: HTTP/2 DATA frame headers, HTTP/3 + QUIC packet
 * overhead, or WebSocket frame headers (with the client→server mask on
 * upload). Byte accounting; medium/high confidence.
 */
function applicationFramingFactor(
  config: OverheadCompensationConfig,
): CompensationFactor | null {
  const C = COMPENSATION_DEFAULTS;
  const payloadBytes = positive(config.params.framePayloadBytes);

  // HTTP/1.1 (cleartext or over TLS) streams the body with no per-DATA-frame
  // header — chunked-encoding markers are negligible over a long stream — so
  // there is no application-framing lift to add.
  if (config.transport === "http1-clear" || config.transport === "https-tls") {
    return null;
  }

  if (isQuic(config)) {
    // HTTP/3 DATA frame overhead × QUIC short-header packetization.
    const quicPayload = Math.min(payloadBytes, C.quicMaxPayloadBytes);
    const quicBytes =
      C.quicShortHeaderBaseBytes +
      positive(config.params.quicConnIdBytes) +
      positive(config.params.aeadTagBytes);
    const h3 = (payloadBytes + C.http3FrameOverheadBytes) / payloadBytes;
    const quic = (quicPayload + quicBytes) / quicPayload;
    return factor(
      "application-framing",
      "HTTP/3 / QUIC framing",
      h3 * quic,
      "medium",
      `${C.http3FrameOverheadBytes} B HTTP/3 frame + ${quicBytes} B QUIC packet estimate`,
    );
  }

  // TCP transfer backend (fetch-stream) → HTTP/2 DATA frames.
  return factor(
    "application-framing",
    "HTTP/2 DATA frames",
    (payloadBytes + C.http2FrameHeaderBytes) / payloadBytes,
    "high",
    `${C.http2FrameHeaderBytes} B per ${payloadBytes} B DATA frame`,
  );
}

/**
 * TLS records: 5B record header + 1B inner content-type + AEAD tag,
 * amortized over the record payload. Deterministic for TCP/TLS; skipped
 * for QUIC (its packet protection is modeled in application framing).
 */
function tlsRecordFactor(
  config: OverheadCompensationConfig,
): CompensationFactor | null {
  // No TLS records on cleartext HTTP/1.1 or on QUIC (its packet protection is
  // modeled in application framing) — only HTTP/1.1-over-TLS and HTTP/2 carry them.
  if (!usesTls(config)) return null;

  const C = COMPENSATION_DEFAULTS;
  // The amortization denominator is the TLS record PAYLOAD window. `params`
  // exposes `tlsRecordBytes` only as the record-HEADER size knob (≈5); the
  // large application record payload is the frame payload window (≈16 KiB).
  const recordPayload = positive(config.params.framePayloadBytes);
  const overheadBytes =
    C.tlsRecordHeaderBytes +
    C.tlsInnerContentTypeBytes +
    positive(config.params.aeadTagBytes);

  return factor(
    "tls-records",
    "TLS records",
    (recordPayload + overheadBytes) / recordPayload,
    "high",
    `${overheadBytes} B per ${recordPayload} B record`,
  );
}

/**
 * Ethernet/IP/L4 framing: the per-MTU-frame link overhead (preamble,
 * IPG, MAC, FCS, optional VLAN) plus IP + TCP/UDP headers, amortized
 * over the usable transport payload. Fixed protocol accounting.
 */
function ethernetFramingFactor(
  config: OverheadCompensationConfig,
): CompensationFactor {
  const C = COMPENSATION_DEFAULTS;
  const mtuBytes = positive(config.params.mtuBytes);
  const ipBytes =
    config.params.ipVersion === 6 ? C.ipv6HeaderBytes : C.ipv4HeaderBytes;
  const transportBytes = isQuic(config)
    ? C.udpHeaderBytes
    : C.tcpBaseHeaderBytes + Math.max(0, config.params.tcpOptionsBytes);
  const vlanBytes = config.params.vlanTagged ? C.vlanTagBytes : 0;
  const linkBytes =
    C.ethernetPreambleAndGapBytes + C.ethernetMacAndFcsBytes + vlanBytes;
  const transportPayload = Math.max(
    C.minTransportPayloadBytes,
    mtuBytes - ipBytes - transportBytes,
  );

  return factor(
    "ethernet-framing",
    isQuic(config) ? "Ethernet/IP/UDP" : "Ethernet/IP/TCP",
    (mtuBytes + linkBytes) / transportPayload,
    "high",
    `${ipBytes + transportBytes + linkBytes} B overhead at ${mtuBytes} B MTU`,
  );
}

/**
 * Reverse-path control: the access link also carries TCP ACKs (or QUIC
 * control), which consume capacity even though they deliver no payload.
 * Heuristic; low confidence.
 */
function reversePathControlFactor(
  config: OverheadCompensationConfig,
): CompensationFactor {
  const C = COMPENSATION_DEFAULTS;
  if (isQuic(config)) {
    return factor(
      "reverse-path-control",
      "QUIC ACK/control traffic",
      1 + C.quicReversePathRatio,
      "low",
      `${(C.quicReversePathRatio * 100).toFixed(1)}% bidirectional control estimate`,
    );
  }
  const mtuBytes = positive(config.params.mtuBytes);
  const dataWireBytes =
    mtuBytes + C.ethernetPreambleAndGapBytes + C.ethernetMacAndFcsBytes;
  const ratio = C.ackBytesPerTwoPackets / (C.ackPacketsPerData * dataWireBytes);
  return factor(
    "reverse-path-control",
    "TCP ACK traffic",
    1 + ratio,
    "low",
    "one minimum ACK per two data packets",
  );
}

/**
 * VPN-tunnel encapsulation: a WireGuard/Tailscale/OpenVPN tunnel wraps each inner
 * frame in an outer IP+UDP+protocol header (~60 B IPv4 / ~80 B IPv6 for
 * WireGuard), so the physical wire carries that much more per MTU-sized frame.
 * Byte accounting over the (already smaller) tunnel MTU; medium confidence.
 */
function encapsulationFactor(
  config: OverheadCompensationConfig,
): CompensationFactor | null {
  const mtuBytes = positive(config.params.mtuBytes);
  const encapBytes = Math.max(0, config.params.encapsulationBytes);
  if (encapBytes <= 0) return null;
  return factor(
    "encapsulation",
    "VPN tunnel encapsulation",
    (mtuBytes + encapBytes) / mtuBytes,
    "medium",
    `${encapBytes} B outer header per ${mtuBytes} B tunnel frame`,
  );
}

/**
 * Receiver-bias (DOWNLOAD only): the browser receiver is CPU-bound and times its
 * rate over wall-dt, so sub-watchdog micro-pauses (GC, event-loop, receive cost)
 * sit inside the measured interval and depress the counted rate — and the deficit
 * GROWS with throughput. Upload is measured by the active-timed Go receiver, which
 * is immune, so this is asymmetric: it lifts the download estimate, never upload.
 * A saturating (Michaelis–Menten) curve, ≈0 at low rate → ceil at multi-gigabit.
 * This is a MEASUREMENT correction, not wire bytes; low confidence.
 */
function receiverBiasFactor(
  phase: CompensationPhase,
  rate: number,
  _config: OverheadCompensationConfig,
): CompensationFactor | null {
  if (phase !== "download") return null;
  const C = COMPENSATION_DEFAULTS;
  const r = Math.max(0, rate);
  const lift =
    (C.receiverBiasCeilRatio * r) / (r + C.receiverBiasHalfRateBytes);
  if (lift <= C.receiverBiasMinLift) return null;
  return factor(
    "receiver-bias",
    "Browser receive cost (download)",
    1 + lift,
    "low",
    `${(lift * 100).toFixed(1)}% browser receive-side measurement bias at this rate`,
  );
}

/**
 * Loss / retransmission tax ≈ loss/(1−loss), capped by `maxLossRatio`.
 * Heuristic — browsers don't expose retransmitted wire bytes, so the under-load
 * ping loss on `ThroughputResult.packetLossPct` is used as the proxy. Invoked by
 * `estimateResultCompensation` when the loss factor is enabled.
 */
export function lossRetransmissionFactor(
  packetLossRatio: number,
  config: OverheadCompensationConfig,
): CompensationFactor | null {
  const lossRatio = Math.min(
    Math.max(0, packetLossRatio),
    config.params.maxLossRatio,
  );
  if (lossRatio <= 0) return null;
  return factor(
    "loss-retransmission",
    "Loss / retransmission",
    1 / (1 - lossRatio),
    "low",
    `${(lossRatio * 100).toFixed(2)}% observed loss, capped`,
  );
}

/**
 * Steady-state ramp: early-test ramp-up drags the full-run AVERAGE below
 * the attainable PLATEAU. `ThroughputResult` carries no sample array, so
 * this approximates the plateau from the aggregate proxy peak-vs-mean,
 * blended by the plateau percentile (a peak overstates the plateau, so we
 * blend toward it rather than taking the raw peak). Heuristic; low
 * confidence.
 */
function steadyStateRampFactor(
  result: ThroughputResult,
): CompensationFactor | null {
  const C = COMPENSATION_DEFAULTS;
  if (
    result.meanBytesPerSec <= 0 ||
    result.peakBytesPerSec <= result.meanBytesPerSec
  )
    return null;

  // Blend mean→peak by the plateau percentile: the "plateau" sits between
  // the average and the peak, approximated at the p65 fraction of that span.
  const plateau =
    result.meanBytesPerSec +
    (result.peakBytesPerSec - result.meanBytesPerSec) *
      C.steadyStatePlateauPercentile;
  const rawLift = plateau / result.meanBytesPerSec - 1;
  const lift = Math.min(Math.max(0, rawLift), maxLift(result));
  if (lift <= C.steadyStateMinLift) return null;

  return factor(
    "steady-state-ramp",
    "Steady-state ramp",
    1 + lift,
    "low",
    `plateau is ${(lift * 100).toFixed(1)}% above run average`,
  );
}

/** Cap the ramp lift at the maxSteadyStateLift implied by stability. */
function maxLift(result: ThroughputResult): number {
  // A perfectly stable run (stabilityPct → 100) admits no ramp lift; a noisy
  // one admits more. Use the instability fraction as the lift ceiling.
  return Math.max(0, 1 - result.stabilityPct / 100);
}

/**
 * Browser-runtime tax: event-loop, stream-reader, GC, and rendering jitter
 * depress measured throughput below the network's. The tax is the sample
 * coefficient-of-variation × 0.08 (`runtimeVarianceWeight`). We derive CoV
 * from `stabilityPct` (a CoV-based 0–100 score; CoV ≈ 1 − stability/100).
 * Heuristic; low confidence.
 */
function browserRuntimeFactor(
  result: ThroughputResult,
): CompensationFactor | null {
  const C = COMPENSATION_DEFAULTS;
  const cov = Math.max(0, 1 - result.stabilityPct / 100);
  const tax = Math.max(0, cov * C.runtimeVarianceWeight);
  if (tax <= C.runtimeMinTax) return null;
  return factor(
    "browser-runtime",
    "Browser/runtime tax",
    1 + tax,
    "low",
    `${(tax * 100).toFixed(1)}% timing and stream overhead estimate`,
  );
}

/* ============================================================
 * HELPERS
 * ============================================================ */

function factor(
  key: CompensationFactor["key"],
  label: string,
  multiplier: number,
  confidence: CompensationConfidence,
  detail: string,
): CompensationFactor {
  return { key, label, multiplier, ratio: multiplier - 1, confidence, detail };
}

/** Fall back to 1 for non-positive/NaN inputs so a ratio never divides by 0. */
function positive(value: number): number {
  return Number.isFinite(value) && value > 0 ? value : 1;
}

/** Worst confidence wins: any low → low, else any medium → medium, else high. */
function combinedConfidence(
  factors: CompensationFactor[],
): CompensationConfidence {
  if (!factors.length) return "high";
  if (factors.some((f) => f.confidence === "low")) return "low";
  if (factors.some((f) => f.confidence === "medium")) return "medium";
  return "high";
}
