// Holds the Settings rules that need no page, from stepper arithmetic to typed values and presets.
import { DURATION_PRESETS } from "../../app/preferences";
import type { TestConfig } from "../../engine/types";

export type Preset = keyof typeof DURATION_PRESETS;
export const PRESETS = Object.keys(DURATION_PRESETS) as Preset[];
export type Direction = 1 | -1;

/** A stage's step grows with its time and is chosen from the value being left, so 10 s steps down by 0.5 s and up by 1 s. */
export function stageStepMs(ms: number, direction: Direction): number {
  const from = direction < 0 ? ms - 1 : ms;
  if (from < 10_000) return 500;
  if (from < 60_000) return 1_000;
  if (from < 600_000) return 10_000;
  return from < 3_600_000 ? 60_000 : 300_000;
}

/** Moves one step from `value`, landing on the step's grid, so 10.5 s steps to 11 s or to 10 s. */
export const stepOnGrid = (value: number, size: number, direction: Direction) =>
  direction > 0 ? Math.floor(value / size) * size + size : Math.ceil(value / size) * size - size;

interface Field {
  min: number;
  max: number;
  format(value: number): string;
  parse(text: string): number | null;
}

/** Rounds a typed value to what the field shows, then clamps it last so a server's limit is never passed; non-values keep `current`. */
export function typedValue(text: string, current: number, field: Field): number {
  const typed = field.parse(text);
  if (typed === null) return current;
  return Math.min(field.max, Math.max(field.min, field.parse(field.format(typed)) ?? current));
}

/** A typed stream count is a whole number, as `Number` reads it. */
export function parseCount(text: string): number | null {
  const count = Number(text.trim());
  return text.trim() && Number.isInteger(count) ? count : null;
}

/** Finds the preset whose five times all match, or null. */
export const presetOf = (duration: TestConfig["duration"]): Preset | null =>
  PRESETS.find((preset) =>
    (Object.keys(duration) as (keyof TestConfig["duration"])[]).every(
      (key) => DURATION_PRESETS[preset][key] === duration[key],
    ),
  ) ?? null;
