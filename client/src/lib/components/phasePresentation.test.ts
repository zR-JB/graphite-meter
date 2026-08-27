import { test, expect } from "bun:test";
import { phaseKicker, phaseMessage } from "./phasePresentation";
import type { Phase } from "../runner/contract";

test("phaseKicker: one eyebrow per phase, idle falls back to Standby", () => {
  for (const [phase, expected] of [
    ["connecting", "Connecting"], ["bidirectional", "Bidirectional"], ["error", "Error"], ["idle", "Standby"],
  ] as const)
    expect(phaseKicker(phase)).toBe(expected);
});

test("phaseMessage: plain-language copy per phase", () => {
  for (const [phase, expected] of [
    ["latency", "Measuring path latency"], ["bidirectional", "Sending + receiving"], ["aborted", "Sequence stopped"], ["idle", "Ready"],
  ] as const)
    expect(phaseMessage(phase, null)).toBe(expected);
});

test("phaseMessage: error prefers the resolved reason label", () => {
  expect(phaseMessage("error", "TLS handshake failed")).toBe("TLS handshake failed");
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
