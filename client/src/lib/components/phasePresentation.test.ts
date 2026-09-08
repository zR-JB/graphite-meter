import { test, expect } from "bun:test";
import {
  completionLabel,
  phaseKicker,
  phaseMessage,
} from "./phasePresentation";
import type { Phase } from "../runner/contract";

test("phaseMessage: error prefers the resolved reason label", () => {
  expect(phaseMessage("error", "TLS handshake failed")).toBe(
    "TLS handshake failed",
  );
  expect(phaseMessage("error", null)).toBe("Runner needs attention");
});

test("terminal labels preserve the measured result outcome", () => {
  for (const [outcome, label] of [
    [undefined, "Complete"],
    ["complete", "Complete"],
    ["partial", "Partial"],
    ["incomplete", "Incomplete"],
  ] as const) {
    expect(completionLabel(outcome)).toBe(label);
    expect(phaseKicker("complete", outcome)).toBe(label);
    expect(phaseMessage("complete", null, outcome)).toBe(label);
  }
});

test("result outcomes do not change other phase labels", () => {
  const phases: Exclude<Phase, "complete">[] = [
    "idle",
    "connecting",
    "warmup",
    "latency",
    "download",
    "upload",
    "bidirectional",
    "aborted",
    "error",
  ];
  for (const phase of phases) {
    for (const outcome of ["complete", "partial", "incomplete"] as const) {
      expect(phaseKicker(phase, outcome)).toBe(phaseKicker(phase));
      expect(phaseMessage(phase, "Connection lost", outcome)).toBe(
        phaseMessage(phase, "Connection lost"),
      );
    }
  }
});
