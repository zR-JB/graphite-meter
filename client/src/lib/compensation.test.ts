import { expect, test } from "bun:test";
import {
  combineCompensationEstimates,
  compensationTooltip,
  estimateLiveCompensation,
  estimateResultCompensation,
  transportFromProtocol,
} from "./compensation";
import type { ThroughputResult } from "./runner/contract";

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
    probeTimeoutPct: 12,
  };
}

test("1500 B Ethernet defaults map expected TCP goodput to wire occupancy", () => {
  const measured = 2_500_000_000 / 8 / ((1500 + 38) / (1500 - 20 - 20 - 12));
  const estimate = estimateLiveCompensation(
    measured,
    "upload",
    "http/1.1",
    false,
  );
  expect(estimate.path).toBe("ethernet");
  expect(estimate.estimatedBytesPerSec * 8).toBeCloseTo(2_500_000_000, -3);
  expect(estimate.totalMultiplier).toBeCloseTo(1.06215, 4);
  expect(estimate.mtuBytes).toBe(1500);
});

test("the automatic TCP model reports its conservative option range", () => {
  const estimate = estimateLiveCompensation(2_350_000_000 / 8, "download");
  expect(estimate.lowerBytesPerSec).toBeLessThan(estimate.estimatedBytesPerSec);
  expect(estimate.upperBytesPerSec).toBe(estimate.estimatedBytesPerSec);
  expect(estimate.lowerBytesPerSec * 8).toBeGreaterThan(2_470_000_000);
  expect(estimate.upperBytesPerSec * 8).toBeLessThan(2_500_000_000);
});

test("result stability, peak, and ping loss never change wire accounting", () => {
  const estimate = estimateResultCompensation(
    result(2_260_000_000 / 8),
    "download",
  );
  expect(estimate.estimatedBytesPerSec * 8).toBeCloseTo(2_400_000_000, -7);
  expect(estimate.factors.map((factor) => factor.key)).toEqual([
    "ethernet",
    "ip",
    "transport",
  ]);
});

test("HTTP/1 TLS retains its record overhead regression", () => {
  const clear = estimateLiveCompensation(
    1_000_000,
    "download",
    "http/1.1",
    false,
  );
  const tls = estimateLiveCompensation(1_000_000, "download", "http/1.1", true);
  expect(tls.totalMultiplier / clear.totalMultiplier).toBeCloseTo(
    1 + 22 / 16_384,
    8,
  );
  expect(tls.factors.map((factor) => factor.key)).toContain("tls-records");
});

test("negotiated protocol and security select the automatic transport", () => {
  expect(transportFromProtocol("http/1.1", false)).toBe("http1-clear");
  expect(transportFromProtocol("h2", true)).toBe("http2");
  expect(transportFromProtocol("h3", true)).toBe("http3-quic");
  expect(transportFromProtocol(undefined, true)).toBe("https-tls");
  expect(
    estimateLiveCompensation(1_000_000, "download", "http/1.1", true),
  ).toMatchObject({
    transport: "https-tls",
    transportSource: "detected",
  });
});

test("selected WebTransport mechanism forces QUIC despite an H1/H2 fetch probe", () => {
  const stream = estimateLiveCompensation(
    1_000_000,
    "download",
    "http/1.1",
    false,
    4,
    "192.0.2.1",
    "webtransport",
  );
  const datagram = estimateLiveCompensation(
    1_000_000,
    "download",
    "h2",
    true,
    4,
    "192.0.2.1",
    "webtransport-datagram",
  );
  expect(stream).toMatchObject({
    transport: "http3-quic",
    framing: "webtransport-stream",
  });
  expect(datagram).toMatchObject({
    transport: "http3-quic",
    framing: "webtransport-datagram",
  });
  expect(stream.factors.map((factor) => factor.label)).toContain(
    "WebTransport QUIC stream frames",
  );
  expect(datagram.factors.map((factor) => factor.label)).toContain(
    "WebTransport QUIC datagrams",
  );
  expect(stream.factors.map((factor) => factor.label)).not.toContain(
    "HTTP/3 DATA frames",
  );
  expect(datagram.factors.map((factor) => factor.label)).not.toContain(
    "HTTP/3 DATA frames",
  );
});

test("unknown protocol uses a documented security-aware fallback", () => {
  expect(estimateLiveCompensation(1_000_000, "download")).toMatchObject({
    transport: "http1-clear",
    transportSource: "fallback",
  });
  expect(
    estimateLiveCompensation(1_000_000, "download", undefined, true),
  ).toMatchObject({
    transport: "https-tls",
    transportSource: "fallback",
  });
});

test("authoritative preflight IP family changes only IP overhead", () => {
  const ipv4 = estimateLiveCompensation(
    1_000_000,
    "download",
    "http/1.1",
    false,
    4,
  );
  const ipv6 = estimateLiveCompensation(
    1_000_000,
    "download",
    "http/1.1",
    false,
    6,
  );
  expect(ipv4.ipVersionSource).toBe("detected");
  expect(ipv6.ipVersionSource).toBe("detected");
  expect(ipv6.estimatedBytesPerSec).toBeGreaterThan(ipv4.estimatedBytesPerSec);
  expect(estimateLiveCompensation(1_000_000, "download").ipVersion).toBe(4);
});

test("loopback preflight has no physical wire estimate", () => {
  for (const address of ["127.0.0.1", "127.42.0.9", "::1", "[::1]"]) {
    const estimate = estimateLiveCompensation(
      1_000_000,
      "download",
      "h2",
      true,
      4,
      address,
    );
    expect(estimate.available).toBe(false);
    expect(estimate.path).toBe("loopback");
    expect(estimate.totalMultiplier).toBe(1);
  }
});

test("non-loopback preflight keeps the physical estimate available", () => {
  expect(
    estimateLiveCompensation(1_000_000, "download", "h2", true, 4, "192.0.2.1")
      .available,
  ).toBe(true);
});

test("bidirectional compensation is the sum of independently modeled lanes", () => {
  const down = estimateLiveCompensation(2_000_000, "download", "h2", true, 4);
  const up = estimateLiveCompensation(500_000, "upload", "h2", true, 4);
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
  expect(combined.path).toBe("ethernet");
});

test("factor contributions sum to the displayed overhead", () => {
  for (const protocol of ["http/1.1", "h2", "h3"] as const)
    for (const ipVersion of [4, 6] as const) {
      const estimate = estimateLiveCompensation(
        1_000_000,
        "download",
        protocol,
        true,
        ipVersion,
      );
      expect(
        estimate.factors.reduce(
          (sum, factor) => sum + factor.contributionPct,
          0,
        ),
      ).toBeCloseTo((estimate.totalMultiplier - 1) * 100, 10);
    }
});

test("tooltip reports automatic assumptions and loopback boundary", () => {
  const estimate = estimateLiveCompensation(
    1_000_000,
    "download",
    "h3",
    true,
    6,
    "2001:db8::1",
  );
  expect(compensationTooltip(estimate)).toContain("IPv6 detected · MTU 1500 B");
  expect(compensationTooltip(estimate)).toContain("UDP + QUIC");
  expect(compensationTooltip(estimate)).toContain(
    "Local Ethernet · HTTP/3 QUIC · assumed",
  );
  expect(compensationTooltip(estimate)).toContain("Transport detected");
  expect(compensationTooltip(estimate)).toContain("Total +");
  const loopback = estimateLiveCompensation(
    1_000_000,
    "download",
    "h2",
    true,
    4,
    "127.0.0.1",
  );
  expect(compensationTooltip(loopback)).toBe(
    "Wire n/a\nLoopback · No physical-link estimate applies",
  );
});
