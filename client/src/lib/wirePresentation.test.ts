import { expect, test } from "bun:test";
import { presentWireEstimate } from "./wirePresentation";
import type { CompensationEstimate } from "./compensation";

const estimate: CompensationEstimate = {
  measuredBytesPerSec: 100,
  estimatedBytesPerSec: 110,
  lowerBytesPerSec: 108,
  upperBytesPerSec: 112,
  totalMultiplier: 1.1,
  confidence: "medium",
  factors: [],
  profile: "tunnel",
  transport: "http3-quic",
  assumptions: ["IPv6, 1500 B MTU", "0–20 B QUIC connection ID"],
  available: true,
};

test("wire presentation labels the estimate and exposes the complete model", () => {
  const view = presentWireEstimate(estimate, (rate) => `${rate} B/s`);
  expect(view.kind).toBe("estimate");
  expect(view.text).toBe("≈ 110 B/s wire estimate");
  expect(view.tooltip).toContain("Modeled range: 108 B/s–112 B/s");
  expect(view.tooltip).toContain("Multiplier: ×1.1000");
  expect(view.tooltip).toContain("Profile: tunnel; transport: HTTP/3 QUIC");
  expect(view.tooltip).toContain("Excludes ACK traffic");
});

test("loopback is explicitly unavailable rather than an identity estimate", () => {
  const view = presentWireEstimate(
    {
      ...estimate,
      profile: "loopback",
      available: false,
      totalMultiplier: 1,
    },
    String,
  );
  expect(view.kind).toBe("unavailable");
  expect(view.text).toContain("no physical-wire estimate");
});
