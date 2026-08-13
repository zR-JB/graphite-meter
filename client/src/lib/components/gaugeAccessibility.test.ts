import { expect, test } from "bun:test";
import { authoritativeTransferAnnouncement } from "./gaugeAccessibility";

test("live transfer announcement formats the authoritative rate independently of a visual target", () => {
  const authoritative = 1_000;
  const visualOnlyHint = 1_250;

  const announcement = authoritativeTransferAnnouncement({
    authoritativeBytesPerSec: authoritative,
    visualBytesPerSec: visualOnlyHint,
    toUnit: (bytesPerSec) => bytesPerSec,
    unit: "B/s",
  });

  expect(announcement).toEqual({ value: "1000", unit: "B/s" });
  expect(announcement.value).not.toBe(String(visualOnlyHint));
});
