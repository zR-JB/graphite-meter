import { test, expect } from "bun:test";
import { serverLoadSummary } from "./endpointInfo";

test("occupancy reads as slots and cautions only past half", () => {
  expect(serverLoadSummary({ active: 1, max: 2 })).toBe("1 of 2 slots");
  expect(serverLoadSummary({ active: 3, max: 4 })).toBe(
    "3 of 4 slots · server busy — results may be affected",
  );
});

// A server with no measurement slots configured is neither idle nor busy: the
// ratio is not a number, so every comparison on it is false and the row would
// sit at "0 of 0 slots" for the life of the drawer. The row is dropped instead.
test("a server with no slots configured reports no occupancy", () => {
  expect(serverLoadSummary({ active: 0, max: 0 })).toBeNull();
  expect(serverLoadSummary(undefined)).toBeNull();
});

// An idle server still has occupancy to report: only a missing pool drops the
// row, so the guard above cannot be widened into "hide it when nobody is here".
test("an idle server with a configured pool still reports its slots", () => {
  expect(serverLoadSummary({ active: 0, max: 4 })).toBe("0 of 4 slots");
});
