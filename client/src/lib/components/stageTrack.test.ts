import { test, expect } from "bun:test";
import {
  stageIndex,
  progressFill,
  segmentState,
  bidirectionalState,
  lockReason,
} from "./stageTrack";

test("stageIndex: order and the null sentinel", () => {
  expect(stageIndex("latency")).toBe(0);
  expect(stageIndex("bidirectional")).toBe(3);
  expect(stageIndex(null)).toBe(-1);
});

test("progressFill: fraction to a half-rounded percentage", () => {
  expect(progressFill(0)).toBe(0);
  expect(progressFill(1)).toBe(100);
  expect(progressFill(0.5)).toBe(50);
  expect(progressFill(0.667)).toBe(66.5);
});

test("segmentState: disabled and failed short-circuit before phase", () => {
  expect(segmentState("download", 1, "download", false, false, 1)).toEqual({
    state: "disabled",
    fill: 0,
  });
  expect(segmentState("download", 1, "download", true, true, 1)).toEqual({
    state: "failed",
    fill: 0,
  });
});

test("segmentState: complete fills every enabled segment", () => {
  expect(segmentState("complete", 0, "latency", true, false, -1)).toEqual({
    state: "done",
    fill: 100,
  });
});

test("segmentState: warmup marks earlier done, current warming, later pending", () => {
  // curI = index of the download stage.
  const curI = stageIndex("download");
  expect(segmentState("warmup", 0, "latency", true, false, curI).state).toBe(
    "done",
  );
  expect(segmentState("warmup", 0, "download", true, false, curI).state).toBe(
    "warmup",
  );
  expect(segmentState("warmup", 0, "upload", true, false, curI).state).toBe(
    "pending",
  );
});

test("segmentState: running fills the active stage from the fraction", () => {
  const curI = stageIndex("download");
  expect(segmentState("download", 0.25, "latency", true, false, curI)).toEqual({
    state: "done",
    fill: 100,
  });
  expect(segmentState("download", 0.25, "download", true, false, curI)).toEqual(
    { state: "active", fill: 25 },
  );
  expect(segmentState("download", 0.25, "upload", true, false, curI)).toEqual({
    state: "pending",
    fill: 0,
  });
});

test("segmentState: no active stage yet leaves everything pending", () => {
  expect(segmentState("connecting", 0, "latency", true, false, -1)).toEqual({
    state: "pending",
    fill: 0,
  });
});

test("bidirectionalState: disabled yields no segment", () => {
  expect(
    bidirectionalState("bidirectional", 0.5, "bidirectional", false, false),
  ).toBeNull();
});

test("bidirectionalState: failure, complete, warmup, active, pending", () => {
  expect(
    bidirectionalState("bidirectional", 0, "bidirectional", true, true)?.state,
  ).toBe("failed");
  expect(bidirectionalState("complete", 0, null, true, false)?.state).toBe(
    "done",
  );
  expect(
    bidirectionalState("warmup", 0, "bidirectional", true, false)?.state,
  ).toBe("warmup");
  expect(
    bidirectionalState("bidirectional", 0.5, "bidirectional", true, false),
  ).toEqual({ state: "active", fill: 50 });
  expect(
    bidirectionalState("download", 0, "download", true, false)?.state,
  ).toBe("pending");
});

test("lockReason: freely toggleable returns null", () => {
  expect(lockReason(true, "idle", null, "download", "pending")).toBeNull();
});

test("lockReason: a finished segment reads as done", () => {
  expect(lockReason(false, "upload", "upload", "download", "done")).toBe(
    "done",
  );
});

test("lockReason: the running stage reads as running", () => {
  expect(lockReason(false, "download", "download", "download", "active")).toBe(
    "running",
  );
});

test("lockReason: earlier stage done, later stage upcoming", () => {
  // Current phase stage is upload; download precedes it, upload is not yet past.
  expect(lockReason(false, "upload", "upload", "download", "pending")).toBe(
    "done",
  );
  expect(lockReason(false, "download", "download", "upload", "pending")).toBe(
    "upcoming",
  );
});
