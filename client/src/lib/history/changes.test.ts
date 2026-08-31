import { expect, test } from "bun:test";
import {
  isHistoryChange,
  isHistoryGeneration,
  isRepairHistoryGeneration,
} from "./changes";

test("history change validation accepts only bounded protocol values", () => {
  expect(
    isHistoryChange({
      type: "clear",
      generation: "550e8400-e29b-41d4-a716-446655440000",
    }),
  ).toBe(true);
  expect(
    isHistoryChange({
      type: "put",
      id: "00000000-0000-4000-8000-000000000127",
    }),
  ).toBe(true);
  expect(isHistoryChange({ type: "put" })).toBe(false);
  expect(isHistoryChange({ type: "put", id: "not-a-uuid" })).toBe(false);
  expect(isHistoryChange({ type: "clear", generation: "x".repeat(129) })).toBe(
    false,
  );
  expect(isHistoryChange({ type: "clear", generation: "" })).toBe(false);
  expect(isHistoryChange({ type: "clear", generation: "new", extra: 1 })).toBe(
    false,
  );
});

test("history generation validation keeps corrupt storage inert", () => {
  expect(isHistoryGeneration("")).toBe(true);
  expect(isHistoryGeneration("safe-generation_1.2")).toBe(true);
  expect(isHistoryGeneration("x".repeat(129))).toBe(false);
  expect(isHistoryGeneration("<script>")).toBe(false);
  expect(isHistoryGeneration({})).toBe(false);
  expect(
    isRepairHistoryGeneration("repair-00000000-0000-4000-8000-000000000127"),
  ).toBe(true);
  expect(
    isRepairHistoryGeneration("clear-00000000-0000-4000-8000-000000000127"),
  ).toBe(false);
});
