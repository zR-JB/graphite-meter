import { expect, test } from "bun:test";
import { UploadPresentationBridge } from "./uploadPresentationBridge";

function settled(bridge: UploadPresentationBridge): void {
  for (const at of [0, 100, 200, 300]) bridge.authoritative(1_000, true, at);
}

test("bridge activates only for a fresh local hint after an irregular gap", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  bridge.hint(500, 100, 610);
  expect(bridge.target(610, true)).toBe(1_250);
  expect(bridge.target(610, false)).toBeNull();
  expect(bridge.target(900, true)).toBeNull();
});

test("an authoritative advance wins immediately and clamps no history", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  bridge.hint(10_000, 100, 610);
  expect(bridge.target(610, true)).toBe(1_250);
  bridge.authoritative(900, true, 620);
  expect(bridge.target(1_000, true)).toBeNull();
});
