import { expect, test } from "bun:test";
import { DURATION_PRESETS } from "../../app/preferences";
import { fmtStageTime, parseDuration } from "../../view/format";
import {
  parseCount,
  presetOf,
  PRESETS,
  stageStepMs,
  stepOnGrid,
  typedValue,
  type Direction,
} from "./model";

const S = 1_000;
const MIN = 60 * S;
const H = 60 * MIN;

test("a stage time steps on its grid, by a size chosen from the value it leaves", () => {
  const cases: [number, Direction, number][] = [
    [10 * S, -1, 9.5 * S],
    [10 * S, 1, 11 * S],
    [10.5 * S, 1, 11 * S],
    [10.5 * S, -1, 10 * S],
    [0.7 * S, 1, 1 * S],
    [59 * S, 1, MIN],
    [MIN, -1, 59 * S],
    [MIN, 1, MIN + 10 * S],
    [10 * MIN, 1, 11 * MIN],
    [H, -1, 59 * MIN],
    [H, 1, H + 5 * MIN],
  ];
  for (const [ms, direction, next] of cases)
    expect(stepOnGrid(ms, stageStepMs(ms, direction), direction), `${ms} ${direction}`).toBe(next);
});

test("typed text rounds to what the field shows, then meets its limits, last", () => {
  const stage = { min: S, max: 5 * MIN, format: fmtStageTime, parse: parseDuration };
  const warmup = { ...stage, min: 0, max: 4 * S };
  const streams = { min: 1, max: 128, format: String, parse: parseCount };
  const cases: [typeof stage, string, number][] = [
    [stage, "2", 2 * S],
    [stage, "1:30", 90 * S],
    [stage, "59.96", MIN],
    [stage, "10 min", 5 * MIN],
    [stage, "0.2", S],
    [stage, "abc", 4 * S],
    [stage, "", 4 * S],
    [warmup, "1.23", 1.2 * S],
    [streams, "12", 12],
    [streams, "1e2", 100],
    [streams, "0x10", 16],
    [streams, "500", 128],
    [streams, "2.5", 4 * S],
    [streams, " ", 4 * S],
  ];
  for (const [field, text, value] of cases)
    expect(typedValue(text, 4 * S, field), text).toBe(value);
});

test("a preset is pressed only while all five times match it", () => {
  for (const preset of PRESETS) {
    expect(presetOf({ ...DURATION_PRESETS[preset] })).toBe(preset);
    expect(
      presetOf({ ...DURATION_PRESETS[preset], warmupMs: DURATION_PRESETS[preset].warmupMs + 1 }),
    ).toBeNull();
  }
});
