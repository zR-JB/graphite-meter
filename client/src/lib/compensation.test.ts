import { test, expect } from "bun:test";
import {
  estimateResultCompensation,
  estimateLiveCompensation,
  applyConnectionProfile,
  lossRetransmissionFactor,
} from "./compensation";
import type {
  CompensationTransport,
  ConnectionProfile,
  OverheadCompensationConfig,
  ThroughputResult,
} from "./runner/contract";

function makeConfig(
  overrides: {
    enabled?: boolean;
    profile?: ConnectionProfile;
    transport?: CompensationTransport;
    factors?: Partial<OverheadCompensationConfig["factors"]>;
    params?: Partial<OverheadCompensationConfig["params"]>;
  } = {},
): OverheadCompensationConfig {
  return {
    enabled: overrides.enabled ?? true,
    profile: overrides.profile ?? "lan",
    transport: overrides.transport ?? "http2",
    factors: {
      ethernetFraming: true,
      encapsulation: false,
      tlsRecords: true,
      applicationFraming: true,
      reversePathControl: true,
      lossRetransmission: true,
      receiverBias: true,
      steadyStateRamp: true,
      browserRuntime: true,
      ...overrides.factors,
    },
    params: {
      mtuBytes: 1500,
      ipVersion: 4,
      vlanTagged: false,
      tcpOptionsBytes: 12,
      encapsulationBytes: 0,
      framePayloadBytes: 16384,
      tlsRecordBytes: 5,
      aeadTagBytes: 16,
      quicConnIdBytes: 8,
      maxLossRatio: 0.1,
      ...overrides.params,
    },
  };
}

function makeResult(
  overrides: Partial<ThroughputResult> = {},
): ThroughputResult {
  return {
    meanBytesPerSec: 1_000_000,
    peakBytesPerSec: 1_200_000,
    stabilityPct: 80,
    totalBytes: 100_000_000,
    reportedBytesPerSec: 1_000_000,
    fullAverageBytesPerSec: 1_000_000,
    method: "stable-window",
    stabilityScore: 0.8,
    band: "high",
    packetLossPct: 1,
    ...overrides,
  };
}

function keys(estimate: { factors: { key: string }[] }): string[] {
  return estimate.factors.map((f) => f.key);
}

/* ---------- estimateLiveCompensation ---------- */

test("estimateLiveCompensation: zero rate returns the identity estimate", () => {
  const est = estimateLiveCompensation(0, makeConfig(), "download");
  expect(est.totalMultiplier).toBe(1);
  expect(est.estimatedBytesPerSec).toBe(0);
  expect(est.factors).toEqual([]);
});

test("estimateLiveCompensation: disabled config returns identity even at a hot rate", () => {
  const est = estimateLiveCompensation(
    1_000_000_000,
    makeConfig({ enabled: false }),
    "download",
  );
  expect(est.totalMultiplier).toBe(1);
  expect(est.estimatedBytesPerSec).toBe(1_000_000_000);
});

test("estimateLiveCompensation: near-zero positive rate stays finite (no NaN, no div-by-zero)", () => {
  const est = estimateLiveCompensation(1e-9, makeConfig(), "download");
  expect(Number.isFinite(est.totalMultiplier)).toBe(true);
  expect(Number.isFinite(est.estimatedBytesPerSec)).toBe(true);
  expect(Number.isNaN(est.totalMultiplier)).toBe(false);
});

test("estimateLiveCompensation: typical mid-range rate lifts the estimate above measured", () => {
  const est = estimateLiveCompensation(50_000_000, makeConfig(), "download");
  expect(est.totalMultiplier).toBeGreaterThan(1);
  expect(est.estimatedBytesPerSec).toBeGreaterThan(est.measuredBytesPerSec);
  // The live path never walks sample arrays, so these never appear here.
  expect(keys(est)).not.toContain("steady-state-ramp");
  expect(keys(est)).not.toContain("browser-runtime");
});

test("estimateLiveCompensation: receiver-bias only lifts the download phase, never upload", () => {
  const config = makeConfig();
  const down = estimateLiveCompensation(50_000_000, config, "download");
  const up = estimateLiveCompensation(50_000_000, config, "upload");
  expect(keys(down)).toContain("receiver-bias");
  expect(keys(up)).not.toContain("receiver-bias");
});

/* ---------- estimateResultCompensation ---------- */

test("estimateResultCompensation: null result returns identity", () => {
  const est = estimateResultCompensation(null, "download", makeConfig());
  expect(est.totalMultiplier).toBe(1);
  expect(est.factors).toEqual([]);
});

test("estimateResultCompensation: disabled config returns identity", () => {
  const est = estimateResultCompensation(
    makeResult(),
    "download",
    makeConfig({ enabled: false }),
  );
  expect(est.totalMultiplier).toBe(1);
});

test("estimateResultCompensation: zero measured rate returns identity", () => {
  const est = estimateResultCompensation(
    makeResult({ meanBytesPerSec: 0 }),
    "download",
    makeConfig(),
  );
  expect(est.totalMultiplier).toBe(1);
  expect(est.estimatedBytesPerSec).toBe(0);
});

test("estimateResultCompensation: near-zero measured rate stays finite", () => {
  const est = estimateResultCompensation(
    makeResult({ meanBytesPerSec: 1e-6, peakBytesPerSec: 1e-6 }),
    "download",
    makeConfig(),
  );
  expect(Number.isFinite(est.totalMultiplier)).toBe(true);
  expect(Number.isFinite(est.estimatedBytesPerSec)).toBe(true);
});

test("estimateResultCompensation: typical result reports loss, ramp, and runtime factors", () => {
  const est = estimateResultCompensation(
    makeResult(),
    "download",
    makeConfig(),
  );
  const factorKeys = keys(est);
  expect(factorKeys).toContain("loss-retransmission");
  expect(factorKeys).toContain("steady-state-ramp");
  expect(factorKeys).toContain("browser-runtime");
  expect(Number.isFinite(est.totalMultiplier)).toBe(true);
});

test("estimateResultCompensation: zero packet loss reports no loss-retransmission factor", () => {
  const est = estimateResultCompensation(
    makeResult({ packetLossPct: 0 }),
    "download",
    makeConfig(),
  );
  expect(keys(est)).not.toContain("loss-retransmission");
});

test("estimateResultCompensation: peak at or below mean reports no steady-state-ramp factor", () => {
  const est = estimateResultCompensation(
    makeResult({ peakBytesPerSec: 1_000_000, meanBytesPerSec: 1_000_000 }),
    "download",
    makeConfig(),
  );
  expect(keys(est)).not.toContain("steady-state-ramp");
});

test("estimateResultCompensation: perfectly stable run reports no browser-runtime factor", () => {
  const est = estimateResultCompensation(
    makeResult({ stabilityPct: 100 }),
    "download",
    makeConfig(),
  );
  expect(keys(est)).not.toContain("browser-runtime");
});

/* ---------- applyConnectionProfile ---------- */

test("applyConnectionProfile: loopback disables link-layer factors and maxes out the MTU", () => {
  const preset = applyConnectionProfile("loopback", "http1-clear");
  expect(preset.factors.ethernetFraming).toBe(false);
  expect(preset.factors.reversePathControl).toBe(false);
  expect(preset.factors.receiverBias).toBe(false);
  expect(preset.params.mtuBytes).toBe(65536);
});

test("applyConnectionProfile: tunnel enables encapsulation with WireGuard-sized defaults", () => {
  const preset = applyConnectionProfile("tunnel", "http1-clear");
  expect(preset.factors.encapsulation).toBe(true);
  expect(preset.params.mtuBytes).toBe(1420);
  expect(preset.params.encapsulationBytes).toBe(60);
});

test("applyConnectionProfile: lan enables link-layer and receiver-bias factors at 1500 MTU", () => {
  const preset = applyConnectionProfile("lan", "http1-clear");
  expect(preset.factors.ethernetFraming).toBe(true);
  expect(preset.factors.reversePathControl).toBe(true);
  expect(preset.factors.receiverBias).toBe(true);
  expect(preset.params.mtuBytes).toBe(1500);
  expect(preset.params.encapsulationBytes).toBe(0);
});

test("applyConnectionProfile: http1-clear has neither TLS nor application framing", () => {
  const preset = applyConnectionProfile("lan", "http1-clear");
  expect(preset.factors.tlsRecords).toBe(false);
  expect(preset.factors.applicationFraming).toBe(false);
});

test("applyConnectionProfile: https-tls enables TLS but not application framing", () => {
  const preset = applyConnectionProfile("lan", "https-tls");
  expect(preset.factors.tlsRecords).toBe(true);
  expect(preset.factors.applicationFraming).toBe(false);
});

test("applyConnectionProfile: http2 enables both TLS and application framing with TCP options", () => {
  const preset = applyConnectionProfile("lan", "http2");
  expect(preset.factors.tlsRecords).toBe(true);
  expect(preset.factors.applicationFraming).toBe(true);
  expect(preset.params.tcpOptionsBytes).toBe(12);
});

test("applyConnectionProfile: http3-quic enables application framing without TLS and drops TCP options", () => {
  const preset = applyConnectionProfile("lan", "http3-quic");
  expect(preset.factors.tlsRecords).toBe(false);
  expect(preset.factors.applicationFraming).toBe(true);
  expect(preset.params.tcpOptionsBytes).toBe(0);
});

/* ---------- lossRetransmissionFactor ---------- */

test("lossRetransmissionFactor: zero loss returns null", () => {
  expect(lossRetransmissionFactor(0, makeConfig())).toBeNull();
});

test("lossRetransmissionFactor: negative loss ratio clamps to zero and returns null", () => {
  expect(lossRetransmissionFactor(-0.5, makeConfig())).toBeNull();
});

test("lossRetransmissionFactor: near-zero positive loss produces a small finite multiplier", () => {
  const f = lossRetransmissionFactor(0.0001, makeConfig());
  expect(f).not.toBeNull();
  expect(Number.isFinite(f!.multiplier)).toBe(true);
  expect(f!.multiplier).toBeGreaterThan(1);
  expect(f!.multiplier).toBeCloseTo(1 / (1 - 0.0001), 6);
});

test("lossRetransmissionFactor: loss above maxLossRatio is capped at the ceiling", () => {
  const config = makeConfig({ params: { maxLossRatio: 0.05 } });
  const f = lossRetransmissionFactor(0.5, config);
  expect(f).not.toBeNull();
  expect(f!.multiplier).toBeCloseTo(1 / (1 - 0.05), 6);
});
