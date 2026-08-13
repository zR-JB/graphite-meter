import { test, expect } from "bun:test";
import { segmentState, lockReason, stageTrackModel } from "./stageTrack";
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

test("terminal selection can skip retained execution without rewriting it", () => {
  const execution = stage({ status: "complete", fill: 100 });
  expect(
    stageTrackModel({ selected: false, locked: false, execution }),
  ).toMatchObject({
    selected: false,
    state: "disabled",
    fill: 0,
    tag: "skipped",
    execution,
  });
  expect(
    stageTrackModel({ selected: true, locked: false, execution }),
  ).toMatchObject({
    selected: true,
    state: "complete",
    fill: 100,
    tag: null,
    execution,
  });
});

test("failed and partial execution remain visible when selected after termination", () => {
  for (const status of ["failed", "partial"] as const) {
    expect(
      stageTrackModel({
        selected: true,
        locked: false,
        execution: stage({
          status,
          fill: status === "partial" ? 100 : 0,
          failure: true,
        }),
      }),
    ).toMatchObject({ state: status, tag: status });
  }
});

test("a stage enabled after a retained run is queued only for the next run", () => {
  expect(
    stageTrackModel({
      selected: true,
      locked: false,
      execution: stage({ configured: false, status: "disabled" }),
    }),
  ).toMatchObject({ state: "pending", fill: 0, tag: "next run" });
});

test("future-stage toggles project as skipped while past and current stages stay locked", () => {
  const pending = stage({ status: "pending" });
  expect(
    stageTrackModel({ selected: false, locked: false, execution: pending }),
  ).toMatchObject({
    state: "disabled",
    tag: "skipped",
    locked: false,
  });
  expect(
    stageTrackModel({ selected: true, locked: true, execution: pending }),
  ).toMatchObject({
    state: "pending",
    locked: true,
  });
});
