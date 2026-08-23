import { expect, test } from "bun:test";
import {
  classifySessionCoverage,
  liveScheduleFitsSession,
  requireSessionCoverage,
  sessionBudgetCovers,
} from "./auth";

const hour = 60 * 60 * 1_000;

test("session coverage is disabled when authentication is disabled", async () => {
  expect(await requireSessionCoverage(8 * hour)).toBeNull();
});

test("session coverage handles every lifetime boundary", () => {
  expect(classifySessionCoverage(hour, hour, 8 * hour)).toBe("enough");
  expect(classifySessionCoverage(hour + 1, hour, 8 * hour)).toBe("renew");
  expect(classifySessionCoverage(8 * hour + 1, 8 * hour, 8 * hour)).toBe(
    "too-long",
  );
  const invalid: Array<[number, unknown, unknown]> = [
    [Number.NaN, hour, 8 * hour],
    [hour, undefined, 8 * hour],
    [hour, -1, 8 * hour],
    [hour, hour, "8h"],
    [hour, 9 * hour, 8 * hour],
  ];
  for (const values of invalid)
    expect(classifySessionCoverage(...values)).toBe("invalid");
});

test("live schedule reductions apply while unsafe extensions roll back", () => {
  const budget = {
    remainingMs: 10_000,
    maximumLifetimeMs: 20_000,
    checkedAt: 1_000,
  };
  expect(liveScheduleFitsSession(null, 5_000, 20_000, 1_000, 2_000)).toBe(true);
  expect(liveScheduleFitsSession(budget, 5_000, 4_000, 1_000, 9_000)).toBe(
    true,
  );
  expect(liveScheduleFitsSession(budget, 5_000, 8_000, 1_000, 2_000)).toBe(
    true,
  );
  expect(liveScheduleFitsSession(budget, 5_000, 8_001, 1_000, 2_000)).toBe(
    false,
  );
});

test("live session budget only shrinks against the monotonic clock", () => {
  const budget = {
    remainingMs: 10_000,
    maximumLifetimeMs: 20_000,
    checkedAt: 1_000,
  };
  expect(sessionBudgetCovers(budget, 9_000, 2_000)).toBe(true);
  expect(sessionBudgetCovers(budget, 9_001, 2_000)).toBe(false);
  expect(sessionBudgetCovers(budget, 8_000, 3_000)).toBe(true);
});
