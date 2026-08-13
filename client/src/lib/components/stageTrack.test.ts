import { test, expect } from "bun:test";
import { segmentState, lockReason } from "./stageTrack";
import type { StagePresentation } from "../state/stagePresentation";

const stage = (
  overrides: Partial<StagePresentation> = {},
): StagePresentation => ({
  stage: "download",
  configured: true,
  status: "pending",
  fill: 0,
  warming: false,
  failure: false,
  hasUsableResult: false,
  ...overrides,
});

test("segmentState projects the central stage state without re-deriving it", () => {
  expect(segmentState(stage({ status: "disabled" }))).toEqual({
    state: "disabled",
    fill: 0,
  });
  expect(segmentState(stage({ status: "partial", fill: 100 }))).toEqual({
    state: "partial",
    fill: 100,
  });
  expect(segmentState(stage({ status: "active", warming: true }))).toEqual({
    state: "warmup",
    fill: 0,
  });
});

test("lockReason uses the central terminal and recovery state", () => {
  expect(lockReason(true, "idle", null, "download", "pending")).toBeNull();
  expect(lockReason(false, "upload", "upload", "download", "partial")).toBe(
    "done",
  );
  expect(lockReason(false, "upload", "upload", "upload", "recovering")).toBe(
    "recovering",
  );
  expect(lockReason(false, "download", "download", "upload", "pending")).toBe(
    "upcoming",
  );
});
