import { test, expect } from "bun:test";
import { phaseMessage } from "./phasePresentation";

test("phaseMessage: error prefers the resolved reason label", () => {
  expect(phaseMessage("error", "TLS handshake failed")).toBe(
    "TLS handshake failed",
  );
  expect(phaseMessage("error", null)).toBe("Runner needs attention");
});
