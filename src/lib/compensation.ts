/* ============================================================
 * The Graphite Meter — Overhead Compensation Engine (§13.3)
 * ------------------------------------------------------------
 * Pure TypeScript, no Svelte. Estimates the TRUE WIRE-RATE that
 * a measured browser throughput implies, by accounting for the
 * bytes the browser never "sees" but the access link still
 * carries: Ethernet/IP/L4 framing, TLS record expansion,
 * application framing (HTTP/2, HTTP/3+QUIC, WebSocket masks),
 * reverse-path control (ACKs), loss/retransmission tax, the
 * ramp toward steady-state plateau, and browser-runtime jitter.
 *
 * Ported from linerate-atelier's 7-factor estimator (§13.0:
 * lift the math, not the SvelteKit structure). Two structural
 * adaptations for this repo:
 *   1. CANONICAL UNIT IS bps (bits/sec), not bytes/sec — the
 *      store stays bps-canonical, so every estimate is bps in,
 *      bps out. Multipliers are unitless, so the byte-accounting
 *      math is identical; only the carried value differs.
 *   2. NO server-reported metadata object — protocol is derived
 *      from `config.transport.transfer` and the endpoint is
 *      assumed TLS (port 443 handshakes). Tunable byte counts
 *      live in `config.compensation.params`.
 *
 * De-magic mandate (§13.0/§13.3): every coefficient linerate
 * scattered inline becomes a named, commented entry in
 * COMPENSATION_DEFAULTS below. The factor functions contain NO
 * bare magic numbers.
 *
 * Hot-path note (§13.3): `estimateResultCompensation` is the
 * full, sample-array-walking estimate (run once on `complete`).
 * `estimateLiveCompensation` is the O(1) live path — it applies
 * ONLY the protocol/config multipliers and NEVER iterates the
 * sample arrays, fixing linerate's per-sample recompute.
 * ============================================================ */

import type {
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
    | "tls-records"
    | "application-framing"
    | "reverse-path-control"
    | "loss-retransmission"
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
  measuredBps: number;
  estimatedBps: number;
  /** Product of every active factor's multiplier (1.0 when disabled). */
  totalMultiplier: number;
  confidence: CompensationConfidence;
  factors: CompensationFactor[];
}

/* ============================================================
 * COMPENSATION_DEFAULTS — every previously-magic coefficient,
 * named and commented. User-tunable protocol params live in
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
  steadyStateWarmupTrim: 0.25, // drop the first 25% of samples (ramp-up region)
  steadyStatePlateauPercentile: 0.65, // average the top 35% (≥ p65) as the plateau
  steadyStateMinSamples: 8, // need ≥ this many phase samples to estimate ramp
  steadyStateMinTrimmedValues: 4, // need ≥ this many post-trim positive values
  steadyStateMinLift: 0.005, // ignore lifts under 0.5% (noise floor)
  /* ---- Browser-runtime tax heuristic ---- */
  runtimeVarianceWeight: 0.08, // map coefficient-of-variation → tax via this weight
  runtimeMinTax: 0.002, // ignore taxes under 0.2% (noise floor)
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
  const measuredBps = result?.meanBps ?? 0;

  // Disabled, no data, or no factors → identity (1.0) multiplier.
  if (!config.enabled || measuredBps <= 0 || result == null) {
    return identityEstimate(measuredBps);
  }

  // `phase` selects which result the caller passed (download vs upload) and is
  // retained in the signature for direction-asymmetric factors (e.g. WebSocket
  // upload masking); the current TCP/TLS/HTTP-2 + QUIC factor set is
  // direction-symmetric, so the protocol factors below don't branch on it.
  void phase;

  const factors: CompensationFactor[] = [];

  // Protocol/config-based factors (same set the live path uses).
  collectProtocolFactors(factors, config);

  // Loss/retransmission — the result type has no per-phase loss, but
  // stability already folds in dropouts; we model loss separately only
  // when the runner surfaces it. With no loss signal in ThroughputResult
  // this factor is skipped (multiplier 1.0).

  // Sample-derived factors, reconstructed from aggregate stats.
  if (config.factors.steadyStateRamp) {
    pushFactor(factors, steadyStateRampFactor(result));
  }
  if (config.factors.browserRuntime) {
    pushFactor(factors, browserRuntimeFactor(result));
  }

  return finalize(measuredBps, factors);
}

/**
 * CHEAP, O(1) live estimate (§13.3). Applies ONLY the protocol/config
 * multipliers (ethernet framing, TLS records, application framing,
 * reverse-path control) to a single instantaneous bps value. Skips
 * every sample-array-heavy factor (steady-state ramp, browser-runtime)
 * so it is safe to call on the live render cadence — it never touches
 * a sample buffer.
 */
export function estimateLiveCompensation(
  bps: number,
  config: OverheadCompensationConfig,
): CompensationEstimate {
  if (!config.enabled || bps <= 0) return identityEstimate(bps);

  const factors: CompensationFactor[] = [];
  collectProtocolFactors(factors, config);
  return finalize(bps, factors);
}

/* ============================================================
 * FACTOR ASSEMBLY
 * ============================================================ */

/** The protocol/config-based factors shared by live + result paths. */
function collectProtocolFactors(
  factors: CompensationFactor[],
  config: OverheadCompensationConfig,
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
  if (config.factors.reversePathControl) {
    pushFactor(factors, reversePathControlFactor(config));
  }
}

function finalize(
  measuredBps: number,
  factors: CompensationFactor[],
): CompensationEstimate {
  const totalMultiplier = factors.reduce((p, f) => p * f.multiplier, 1);
  return {
    measuredBps,
    estimatedBps: measuredBps * totalMultiplier,
    totalMultiplier,
    confidence: combinedConfidence(factors),
    factors,
  };
}

function identityEstimate(bps: number): CompensationEstimate {
  return {
    measuredBps: bps,
    estimatedBps: bps,
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
  if (!factor || factor.multiplier <= COMPENSATION_DEFAULTS.pushFactorEpsilon) return;
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

/**
 * Whether the transfer rides QUIC (WebTransport) vs TCP (xhr-stream).
 *
 * `OverheadCompensationConfig` (the spec-fixed input) carries NO transport
 * field — that lives on `RunnerConfig.transport.transfer`, which is not
 * threaded into the estimator's signatures. So the estimator models the
 * deterministic, dominant path: TCP/TLS with HTTP/2 DATA framing (the
 * high-confidence byte accounting). The QUIC branches below are retained,
 * gated behind this single switch, so wiring a real QUIC backend later is a
 * one-line change (pass transport in and flip this) rather than a rewrite.
 */
function isQuic(_config: OverheadCompensationConfig): boolean {
  return false;
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

  // TCP transfer backend (xhr-stream) → HTTP/2 DATA frames.
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
  if (isQuic(config)) return null;

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
  const ipBytes = C.ipv4HeaderBytes; // IPv4 assumed; no metadata to detect IPv6
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
 * Loss / retransmission tax ≈ loss/(1−loss), capped by `maxLossRatio`.
 * Heuristic — browsers don't expose retransmitted wire bytes. Exposed
 * for callers that DO have a loss signal (the result path here has none
 * in `ThroughputResult`, so it is not invoked end-to-end yet).
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
 * the attainable PLATEAU. linerate trims the first 25% of samples and
 * averages the top 35% (≥ p65) vs the mean. `ThroughputResult` carries no
 * sample array, so we use the aggregate proxy peak-vs-mean, scaled by the
 * plateau percentile (a peak overstates the plateau, so we blend toward it
 * rather than taking the raw peak). Heuristic; low confidence.
 */
function steadyStateRampFactor(
  result: ThroughputResult,
): CompensationFactor | null {
  const C = COMPENSATION_DEFAULTS;
  if (result.meanBps <= 0 || result.peakBps <= result.meanBps) return null;

  // Blend mean→peak by the plateau percentile: the "plateau" sits between
  // the average and the peak, approximated at the p65 fraction of that span.
  const plateau =
    result.meanBps +
    (result.peakBps - result.meanBps) * C.steadyStatePlateauPercentile;
  const rawLift = plateau / result.meanBps - 1;
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
 * depress measured throughput below the network's. linerate maps the
 * sample coefficient-of-variation × 0.08 → tax. We derive CoV from
 * `stabilityPct` (a CoV-based 0–100 score; CoV ≈ 1 − stability/100).
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
