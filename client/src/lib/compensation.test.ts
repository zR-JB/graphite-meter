import { expect, test } from "bun:test";
import {
  applyConnectionProfile,
  combineCompensationEstimates,
  estimateLiveCompensation,
  estimateResultCompensation,
  transportFromProtocol,
} from "./compensation";
import type {
  OverheadCompensationConfig,
  ThroughputResult,
} from "./runner/contract";

function config(
  overrides: Partial<OverheadCompensationConfig> = {},
): OverheadCompensationConfig {
  const base = applyConnectionProfile("lan");
  return {
    ...base,
    transport: "http1-clear",
    ...overrides,
    params: { ...base.params, ...overrides.params },
  };
}

function result(meanBytesPerSec: number): ThroughputResult {
  return {
    meanBytesPerSec,
    peakBytesPerSec: meanBytesPerSec * 1.5,
    stabilityPct: 40,
    totalBytes: meanBytesPerSec * 10,
    reportedBytesPerSec: meanBytesPerSec,
    fullAverageBytesPerSec: meanBytesPerSec,
    method: "full-average",
    stabilityScore: 0.4,
    band: "low",
    packetLossPct: 12,
  };
}

test("2.5 Gbit Ethernet ceiling maps from its expected TCP goodput", () => {
  const measured = 2_500_000_000 / 8 / ((1500 + 38) / (1500 - 20 - 20 - 12));
  const estimate = estimateLiveCompensation(measured, config(), "upload");
  expect(estimate.estimatedBytesPerSec * 8).toBeCloseTo(2_500_000_000, -3);
  expect(estimate.totalMultiplier).toBeCloseTo(1.06215, 4);
});

test("uncertain TCP options produce an honest range", () => {
  const estimate = estimateLiveCompensation(
    2_350_000_000 / 8,
    config(),
    "download",
  );
  expect(estimate.lowerBytesPerSec).toBeLessThan(estimate.estimatedBytesPerSec);
  expect(estimate.upperBytesPerSec).toBe(estimate.estimatedBytesPerSec);
  expect(estimate.lowerBytesPerSec * 8).toBeGreaterThan(2_470_000_000);
  expect(estimate.upperBytesPerSec * 8).toBeLessThan(2_500_000_000);
});

test("result stability, peak, and ping loss never change wire accounting", () => {
  const mean = 2_260_000_000 / 8;
  const estimate = estimateResultCompensation(
    result(mean),
    "download",
    config(),
  );
  expect(estimate.estimatedBytesPerSec * 8).toBeCloseTo(2_400_000_000, -7);
  expect(estimate.factors.map((factor) => factor.key)).toEqual([
    "network-framing",
  ]);
});

test("TLS and HTTP/2 add only their specified record and frame bytes", () => {
  const h1 = estimateLiveCompensation(1_000_000, config(), "download");
  const h2 = estimateLiveCompensation(
    1_000_000,
    config({ transport: "http2" }),
    "download",
  );
  expect(h2.totalMultiplier / h1.totalMultiplier).toBeCloseTo(
    (1 + 9 / 16384) * (1 + 22 / 16384),
    8,
  );
});

test("automatic transport maps browser Resource Timing protocols", () => {
  expect(transportFromProtocol("http/1.1", false)).toBe("http1-clear");
  expect(transportFromProtocol("h2", true)).toBe("http2");
  expect(transportFromProtocol("h3", true)).toBe("http3-quic");
  expect(transportFromProtocol(undefined, true)).toBe("https-tls");
  const automatic = config({ transport: "auto" });
  expect(
    estimateLiveCompensation(1_000_000, automatic, "download", "http/1.1", true)
      .transport,
  ).toBe("https-tls");
});

test("invalid expert bounds are clamped and keep the central estimate in range", () => {
  const estimate = estimateLiveCompensation(
    1_000_000,
    config({
      transport: "http3-quic",
      params: {
        ...config().params,
        mtuBytes: -1,
        quicConnIdMinBytes: 20,
        quicConnIdMaxBytes: 0,
      },
    }),
    "download",
  );
  expect(estimate.estimatedBytesPerSec).toBeGreaterThanOrEqual(
    estimate.lowerBytesPerSec,
  );
  expect(estimate.estimatedBytesPerSec).toBeLessThanOrEqual(
    estimate.upperBytesPerSec,
  );
});

test("automatic IP family uses preflight detection while an override wins", () => {
  const automatic = config();
  automatic.params.ipVersion = "auto";
  const ipv4 = estimateLiveCompensation(
    1_000_000,
    automatic,
    "download",
    "http/1.1",
    false,
    4,
  );
  const ipv6 = estimateLiveCompensation(
    1_000_000,
    automatic,
    "download",
    "http/1.1",
    false,
    6,
  );
  expect(ipv6.estimatedBytesPerSec).toBeGreaterThan(ipv4.estimatedBytesPerSec);
  expect(
    estimateLiveCompensation(1_000_000, automatic, "download")
      .estimatedBytesPerSec,
  ).toBe(ipv4.estimatedBytesPerSec);

  automatic.params.ipVersion = 4;
  expect(
    estimateLiveCompensation(
      1_000_000,
      automatic,
      "download",
      "http/1.1",
      false,
      6,
    ).estimatedBytesPerSec,
  ).toBe(ipv4.estimatedBytesPerSec);
});

test("loopback has no physical wire estimate", () => {
  const estimate = estimateLiveCompensation(
    1_000_000,
    { ...applyConnectionProfile("loopback"), transport: "auto" },
    "download",
    "h2",
  );
  expect(estimate.available).toBe(false);
  expect(estimate.totalMultiplier).toBe(1);
});

test("tunnel preset is applied once and remains configurable", () => {
  const preset = applyConnectionProfile("tunnel");
  expect(preset.params.mtuBytes).toBe(1420);
  expect(preset.params.encapsulationBytes).toBe(60);
  const estimate = estimateLiveCompensation(
    1_000_000,
    { ...preset, transport: "http1-clear" },
    "upload",
  );
  expect(
    estimate.factors.filter((factor) => factor.key === "encapsulation"),
  ).toHaveLength(1);
});

test("bidirectional compensation is the sum of independently modeled lanes", () => {
  const down = estimateLiveCompensation(2_000_000, config(), "download");
  const up = estimateLiveCompensation(500_000, config(), "upload");
  const combined = combineCompensationEstimates([down, up]);
  expect(combined.measuredBytesPerSec).toBe(2_500_000);
  expect(combined.estimatedBytesPerSec).toBe(
    down.estimatedBytesPerSec + up.estimatedBytesPerSec,
  );
  expect(combined.lowerBytesPerSec).toBe(
    down.lowerBytesPerSec + up.lowerBytesPerSec,
  );
  expect(combined.upperBytesPerSec).toBe(
    down.upperBytesPerSec + up.upperBytesPerSec,
  );
});
