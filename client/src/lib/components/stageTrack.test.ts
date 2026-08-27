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
const model = (
  execution: StagePresentation,
  selected: boolean,
  locked = false,
) => stageTrackModel({ selected, locked, execution });

test("segmentState projects the central stage state without re-deriving it", () => {
  for (const [state, expected] of [
    [{ status: "disabled" }, { state: "disabled", fill: 0 }],
    [{ status: "partial", fill: 100 }, { state: "partial", fill: 100 }],
    [{ status: "active", warming: true }, { state: "warmup", fill: 0 }],
  ] as const) expect(segmentState(stage(state))).toEqual(expected);
});

test("lockReason uses the central terminal and recovery state", () => {
  for (const [terminal, phase, selected, target, status, expected] of [
    [true, "idle", null, "download", "pending", null],
    [false, "upload", "upload", "download", "partial", "done"],
    [false, "upload", "upload", "upload", "recovering", "recovering"],
    [false, "download", "download", "upload", "pending", "upcoming"],
  ] as const) expect(lockReason(terminal, phase, selected, target, status)).toBe(expected);
});

test("terminal selection can skip retained execution without rewriting it", () => {
  const execution = stage({ status: "complete", fill: 100 });
  expect(model(execution, false)).toMatchObject({
    selected: false,
    state: "disabled",
    fill: 0,
    tag: "skipped",
    execution,
  });
  expect(model(execution, true)).toMatchObject({
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
      model(
        stage({
          status,
          fill: status === "partial" ? 100 : 0,
          failure: true,
        }),
        true,
      ),
    ).toMatchObject({ state: status, tag: status });
  }
});

test("a stage enabled after a retained run is queued only for the next run", () => {
  expect(
    model(stage({ configured: false, status: "disabled" }), true),
  ).toMatchObject({
    state: "pending",
    fill: 0,
    tag: "next run",
  });
});

test("future-stage toggles project as skipped while past and current stages stay locked", () => {
  const pending = stage({ status: "pending" });
  expect(model(pending, false)).toMatchObject({
    state: "disabled",
    tag: "skipped",
    locked: false,
  });
  expect(model(pending, true, true)).toMatchObject({
    state: "pending",
    locked: true,
  });
});
