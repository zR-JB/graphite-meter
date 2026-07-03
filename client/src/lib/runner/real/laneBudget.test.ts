import { test, expect } from "bun:test";
import { laneBudget, BROWSER_CONN_BUDGET } from "./laneBudget";

test("non-fetch-stream transport caps at 2, bounded by the ceiling", () => {
  expect(
    laneBudget({
      kind: "webtransport",
      transfer: ["down"],
      dir: "down",
      needsPing: true,
      ceiling: 6,
    }),
  ).toBe(2);
  expect(
    laneBudget({
      kind: "webtransport",
      transfer: ["down"],
      dir: "down",
      needsPing: false,
      ceiling: 1,
    }),
  ).toBe(1);
});

test("download-only: reserves a ping bus when needed, no upload-progress bus", () => {
  expect(
    laneBudget({
      kind: "fetch-stream",
      transfer: ["down"],
      dir: "down",
      needsPing: true,
      ceiling: 6,
    }),
  ).toBe(BROWSER_CONN_BUDGET - 1); // 5
  expect(
    laneBudget({
      kind: "fetch-stream",
      transfer: ["down"],
      dir: "down",
      needsPing: false,
      ceiling: 6,
    }),
  ).toBe(BROWSER_CONN_BUDGET); // 6
});

test("upload-only: reserves both the ping bus and the upload-progress bus", () => {
  expect(
    laneBudget({
      kind: "fetch-stream",
      transfer: ["up"],
      dir: "up",
      needsPing: true,
      ceiling: 6,
    }),
  ).toBe(BROWSER_CONN_BUDGET - 2); // 4
  expect(
    laneBudget({
      kind: "fetch-stream",
      transfer: ["up"],
      dir: "up",
      needsPing: false,
      ceiling: 6,
    }),
  ).toBe(BROWSER_CONN_BUDGET - 1); // 5
});

test("the ceiling caps a single-direction stage exactly as before (regression)", () => {
  for (let ceiling = 1; ceiling <= 6; ceiling++) {
    const down = laneBudget({
      kind: "fetch-stream",
      transfer: ["down"],
      dir: "down",
      needsPing: true,
      ceiling,
    });
    expect(down).toBe(Math.max(1, Math.min(BROWSER_CONN_BUDGET - 1, ceiling)));
  }
});

test("bidirectional: splits the remaining budget 50/50, odd remainder to down", () => {
  // ping + upload-progress buses reserved ⇒ budget = 6 - 2 = 4 ⇒ 2/2 split.
  const down = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "down",
    needsPing: true,
    ceiling: 6,
  });
  const up = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "up",
    needsPing: true,
    ceiling: 6,
  });
  expect(down).toBe(2);
  expect(up).toBe(2);
  expect(down + up).toBeLessThanOrEqual(BROWSER_CONN_BUDGET);
});

test("bidirectional without a ping bus: budget = 5, odd remainder goes to down", () => {
  const down = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "down",
    needsPing: false,
    ceiling: 6,
  });
  const up = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "up",
    needsPing: false,
    ceiling: 6,
  });
  expect(down).toBe(3);
  expect(up).toBe(2);
  expect(down + up).toBe(5);
});

test("bidirectional: the parallelStreams ceiling applies PER DIRECTION, never to the combined total", () => {
  // ceiling=3: each direction independently capped at 3, same as a standalone
  // download/upload stage would be — NOT the combined total capped at 3.
  for (let ceiling = 1; ceiling <= 6; ceiling++) {
    const down = laneBudget({
      kind: "fetch-stream",
      transfer: ["down", "up"],
      dir: "down",
      needsPing: true,
      ceiling,
    });
    const up = laneBudget({
      kind: "fetch-stream",
      transfer: ["down", "up"],
      dir: "up",
      needsPing: true,
      ceiling,
    });
    expect(down).toBeLessThanOrEqual(ceiling);
    expect(up).toBeLessThanOrEqual(ceiling);
    expect(down).toBeGreaterThanOrEqual(1);
    expect(up).toBeGreaterThanOrEqual(1);
  }
});

test("bidirectional lanes never exceed the physical browser budget minus buses", () => {
  const down = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "down",
    needsPing: true,
    ceiling: 6,
  });
  const up = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "up",
    needsPing: true,
    ceiling: 6,
  });
  expect(down + up).toBeLessThanOrEqual(BROWSER_CONN_BUDGET - 2);
});

test("injectable totalBudget is honored (for exercising non-default browser caps)", () => {
  // budget = totalBudget(10) - buses(1, the always-on upload-progress bus for
  // a bidirectional stage) = 9 ⇒ 5/4 split, remainder to down.
  const down = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "down",
    needsPing: false,
    ceiling: 10,
    totalBudget: 10,
  });
  const up = laneBudget({
    kind: "fetch-stream",
    transfer: ["down", "up"],
    dir: "up",
    needsPing: false,
    ceiling: 10,
    totalBudget: 10,
  });
  expect(down).toBe(5);
  expect(up).toBe(4);
});
