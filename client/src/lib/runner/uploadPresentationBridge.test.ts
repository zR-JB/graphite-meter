import { expect, test } from "bun:test";
import {
  UploadPresentationBridge,
  UPLOAD_PRESENTATION_HINT_MAX_AGE_MS,
  UPLOAD_PRESENTATION_SETTLE_MS,
} from "./uploadPresentationBridge";

function settled(bridge: UploadPresentationBridge): void {
  for (const at of [0, 100, 200, 300]) bridge.authoritative(1_000, true, at);
}

test("bridge activates only for a fresh local hint after an irregular gap", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  bridge.hint(0, 500, 100, 610);
  expect(bridge.target(610, true, 1)).toBe(1_250);
  expect(bridge.target(610, false, 1)).toBeNull();
  expect(bridge.target(900, true, 1)).toBeNull();
});

test("an authoritative advance wins immediately and clamps no history", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  bridge.hint(0, 10_000, 100, 610);
  expect(bridge.target(610, true, 1)).toBe(1_250);
  bridge.authoritative(900, true, 620);
  expect(bridge.target(1_000, true, 1)).toBeNull();
});

test("four independently observed lanes aggregate before the authority clamp", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  for (let lane = 0; lane < 4; lane++) bridge.hint(lane, 25, 100, 610);

  expect(bridge.target(610, true, 4)).toBe(1_000);
});

test("uneven lane rates aggregate and a missing lane cannot be manufactured", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  bridge.hint(0, 10, 100, 610);
  bridge.hint(1, 20, 100, 610);
  bridge.hint(2, 30, 100, 610);
  expect(bridge.target(610, true, 4)).toBeNull();
  bridge.hint(3, 40, 100, 610);

  expect(bridge.target(610, true, 4)).toBe(1_000);
});

test("a stale lane cannot keep an aggregate fallback inflated", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  for (let lane = 0; lane < 4; lane++) bridge.hint(lane, 25, 100, 610);
  expect(bridge.target(610, true, 4)).toBe(1_000);
  bridge.hint(0, 25, 100, 1_400);

  expect(bridge.target(1_400, true, 4)).toBeNull();
});

test("an active bridge settles toward authority instead of snapping off at 250 ms", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  bridge.hint(0, 1_250, 1_000, 610);
  expect(bridge.target(610, true, 1)).toBe(1_250);

  const justExpired = bridge.target(
    610 + UPLOAD_PRESENTATION_HINT_MAX_AGE_MS + 1,
    true,
    1,
  );
  expect(justExpired).toBeGreaterThan(1_000);
  expect(justExpired).toBeLessThanOrEqual(1_250);
  expect(
    bridge.target(
      610 + UPLOAD_PRESENTATION_HINT_MAX_AGE_MS + UPLOAD_PRESENTATION_SETTLE_MS,
      true,
      1,
    ),
  ).toBeNull();
});

test("a generation or phase reset discards every lane and arrival baseline", () => {
  const bridge = new UploadPresentationBridge();
  settled(bridge);
  bridge.hint(0, 1_250, 1_000, 610);
  expect(bridge.target(610, true, 1)).toBe(1_250);

  bridge.stop();
  bridge.authoritative(1_000, true, 700);
  bridge.hint(0, 1_250, 1_000, 710);
  expect(bridge.target(710, true, 1)).toBeNull();
});
