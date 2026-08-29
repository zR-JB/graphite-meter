import { expect, test } from "bun:test";
import { returnToLiveIndicator } from "./returnToLive";

test("return-to-live indicator covers preparation, active phases, and recovery", () => {
  expect(returnToLiveIndicator(true, "idle", false)).toMatchObject({
    icon: "bolt",
    tone: "warmup",
  });
  expect(returnToLiveIndicator(false, "latency", false)).toMatchObject({
    icon: "ping",
    tone: "latency",
  });
  expect(returnToLiveIndicator(false, "download", true)?.label).toContain(
    "recovering",
  );
  expect(returnToLiveIndicator(false, "complete", false)).toBeNull();
});
