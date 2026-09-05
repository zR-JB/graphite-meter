import { expect, test } from "bun:test";
import {
  combineCompensationEstimates,
  compensationTooltip,
  estimateCompensation,
} from "./compensation";
test("1500 B Ethernet defaults map expected TCP goodput to wire occupancy", () => {
  const measured = 2_500_000_000 / 8 / ((1500 + 38) / (1500 - 20 - 20 - 12));
  const estimate = estimateCompensation(measured, "http/1.1", false);
  expect(estimate.estimatedBytesPerSec * 8).toBeCloseTo(2_500_000_000, -3);
  expect(estimate.totalMultiplier).toBeCloseTo(1.06215, 4);
  expect(estimate.mtuBytes).toBe(1500);
});

test("the automatic TCP model reports its conservative option range", () => {
  const estimate = estimateCompensation(2_350_000_000 / 8);
  expect(estimate.lowerBytesPerSec).toBeLessThan(estimate.estimatedBytesPerSec);
  expect(estimate.upperBytesPerSec).toBe(estimate.estimatedBytesPerSec);
  expect(estimate.lowerBytesPerSec * 8).toBeGreaterThan(2_470_000_000);
  expect(estimate.upperBytesPerSec * 8).toBeLessThan(2_500_000_000);
});

test("HTTP/1 TLS retains its record overhead regression", () => {
  const clear = estimateCompensation(1_000_000, "http/1.1", false);
  const tls = estimateCompensation(1_000_000, "http/1.1", true);
  expect(tls.totalMultiplier / clear.totalMultiplier).toBeCloseTo(
    1 + 22 / 16_384,
    8,
  );
  expect(tls.factors.map((factor) => factor.key)).toContain("tls-records");
});

test("negotiated protocol and security select the automatic transport", () => {
  expect(estimateCompensation(1_000_000, "http/1.1", false).transport).toBe(
    "http1-clear",
  );
  expect(estimateCompensation(1_000_000, "h2", true).transport).toBe("http2");
  expect(estimateCompensation(1_000_000, "h3", true).transport).toBe(
    "http3-quic",
  );
  expect(estimateCompensation(1_000_000, undefined, true).transport).toBe(
    "https-tls",
  );
  expect(estimateCompensation(1_000_000, "http/1.1", true)).toMatchObject({
    transport: "https-tls",
    transportSource: "detected",
  });
});

test("selected WebTransport mechanism forces QUIC despite an H1/H2 fetch probe", () => {
  const stream = estimateCompensation(
    1_000_000,
    "http/1.1",
    false,
    4,
    "webtransport",
  );
  const datagram = estimateCompensation(
    1_000_000,
    "h2",
    true,
    4,
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
  expect(estimateCompensation(1_000_000)).toMatchObject({
    transport: "http1-clear",
    transportSource: "fallback",
  });
  expect(estimateCompensation(1_000_000, undefined, true)).toMatchObject({
    transport: "https-tls",
    transportSource: "fallback",
  });
});

test("authoritative preflight IP family changes only IP overhead", () => {
  const ipv4 = estimateCompensation(1_000_000, "http/1.1", false, 4);
  const ipv6 = estimateCompensation(1_000_000, "http/1.1", false, 6);
  expect(ipv4.ipVersionSource).toBe("detected");
  expect(ipv6.ipVersionSource).toBe("detected");
  expect(ipv6.estimatedBytesPerSec).toBeGreaterThan(ipv4.estimatedBytesPerSec);
  expect(estimateCompensation(1_000_000).ipVersion).toBe(4);
});

test("missing throughput has no invented overhead", () => {
  const estimate = estimateCompensation(0, "h2", true, 4);
  expect(estimate.estimatedBytesPerSec).toBe(0);
  expect(estimate.totalMultiplier).toBe(1);
  expect(estimate.factors).toEqual([]);
});

test("bidirectional compensation is the sum of independently modeled lanes", () => {
  const down = estimateCompensation(2_000_000, "h2", true, 4);
  const up = estimateCompensation(500_000, "h2", true, 4);
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

test("factor contributions sum to the displayed overhead", () => {
  for (const protocol of ["http/1.1", "h2", "h3"] as const)
    for (const ipVersion of [4, 6] as const) {
      const estimate = estimateCompensation(
        1_000_000,
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

test("tooltip reports automatic assumptions", () => {
  const estimate = estimateCompensation(1_000_000, "h3", true, 6);
  expect(compensationTooltip(estimate)).toContain("IP: IPv6 (detected)");
  expect(compensationTooltip(estimate)).toContain("MTU: 1,500 bytes (assumed)");
  expect(compensationTooltip(estimate)).toContain(
    "Transport: HTTP/3 QUIC (detected)",
  );
  expect(compensationTooltip(estimate)).toContain(
    "Estimated Ethernet overhead:",
  );
});
