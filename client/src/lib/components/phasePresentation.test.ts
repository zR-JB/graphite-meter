import { test, expect } from "bun:test";
import { phaseKicker, phaseMessage } from "./phasePresentation";
import type { Phase } from "../runner/contract";

test("phaseKicker: one eyebrow per phase, idle falls back to Standby", () => {
  expect(phaseKicker("connecting")).toBe("Connecting");
  expect(phaseKicker("bidirectional")).toBe("Bidirectional");
  expect(phaseKicker("error")).toBe("Error");
  expect(phaseKicker("idle")).toBe("Standby");
});

test("phaseMessage: plain-language copy per phase", () => {
  expect(phaseMessage("latency", null)).toBe("Measuring path latency");
  expect(phaseMessage("bidirectional", null)).toBe("Sending + receiving");
  expect(phaseMessage("aborted", null)).toBe("Sequence stopped");
  expect(phaseMessage("idle", null)).toBe("Ready");
});

test("phaseMessage: error prefers the resolved reason label", () => {
  expect(phaseMessage("error", "TLS handshake failed")).toBe(
    "TLS handshake failed",
  );
  expect(phaseMessage("error", null)).toBe("Runner needs attention");
});

test("phaseKicker/phaseMessage: every phase is covered", () => {
  const phases: Phase[] = [
    "idle",
    "connecting",
    "warmup",
    "latency",
    "download",
    "upload",
    "bidirectional",
    "complete",
    "aborted",
    "error",
  ];
  for (const p of phases) {
    expect(phaseKicker(p).length).toBeGreaterThan(0);
    expect(phaseMessage(p, "x").length).toBeGreaterThan(0);
  }
});
