import { expect, test } from "bun:test";
import { HISTORY_LIMIT } from "./types";
import { retainNewest } from "./repository";
import { historyRecord } from "./test-helpers.test";

test("retention keeps exactly the newest 2,000 records", () => {
  const kept = retainNewest(
    Array.from({ length: HISTORY_LIMIT + 17 }, (_, index) => ({
      ...historyRecord(index),
      completedAt: index,
    })),
  );
  expect(kept).toHaveLength(HISTORY_LIMIT);
  expect(kept[0].completedAt).toBe(HISTORY_LIMIT + 16);
  expect(kept.at(-1)?.completedAt).toBe(17);
});
