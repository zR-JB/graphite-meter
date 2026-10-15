import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import type { HistoryRecord } from "./records";
import { clearedSince, HistoryRefusal, HistoryRepository, pastLimit } from "./repository";

const notRun = { latency: null, download: null, upload: null, bidirectional: null };
const record = (id: string) =>
  ({
    schemaVersion: 5,
    id,
    completedAt: 0,
    build: "v1",
    engine: "e",
    result: {
      ...notRun,
      latencyByStage: notRun,
      addedLatency: null,
      multiServer: {
        selection: [],
        participants: [],
        latencyFocus: "a",
        servers: [],
        intervals: [],
        omittedIntervals: 0,
        failures: [],
      },
      outcome: "complete",
      stages: { latency: "not-run", download: "not-run", upload: "not-run", bidirectional: "not-run" },
      startedAt: 0,
      durationMs: 0,
    },
  }) satisfies HistoryRecord;

test("past the limit the oldest readable rows go, a new one among them when its clock is behind", () => {
  const newest = (from: number) => Array.from({ length: 1_000 }, (_, i) => record(`r${from + i}`));
  // Rows newest first; unreadable ones are never counted or removed.
  const rows = [{ schemaVersion: 6 }, ...newest(0), "junk", ...newest(1_000), record("older"), null, record("new")];
  const past = pastLimit();
  expect(rows.map((row) => past(row)).filter((id) => id !== null)).toEqual(["older", "new"]);
});

test("a write is dropped only when History was cleared since its count was read; a corrupt count reads as none", () => {
  const cases: [unknown, number, boolean][] = [
    [undefined, 0, false],
    [{ key: "clears", value: 2 }, 2, false],
    [{ key: "clears", value: 3 }, 2, true],
    [{ key: "clears", value: 1 }, 2, false],
    [{ key: "clears", value: -1 }, 0, false],
    [{ key: "clears", value: 1.5 }, 0, false],
    [{ key: "clears", value: "3" }, 0, false],
    [{ key: "clears", value: 2 ** 53 }, 0, false],
    [{ key: "clears" }, 0, false],
  ];
  for (const [row, clears, dropped] of cases) expect(clearedSince(row, clears), JSON.stringify(row)).toBe(dropped);
});

/** `indexedDB.open` as the browser answers it, one request at a time, driven by the test. */
interface Opening {
  result?: unknown;
  error?: unknown;
  transaction?: { abort(): void };
  onupgradeneeded?: (event: { oldVersion: number }) => void;
  onsuccess?: () => void;
  onerror?: () => void;
  onblocked?: () => void;
}
let openings: Opening[];
const answer = (opening: Opening, clears: number) => {
  const db = {
    closed: false,
    close: () => void (db.closed = true),
    transaction: () => ({
      objectStore: () => ({
        get() {
          const read = { result: { key: "clears", value: clears }, onsuccess: () => {} };
          queueMicrotask(() => read.onsuccess());
          return read;
        },
      }),
    }),
  };
  opening.result = db;
  opening.onsuccess!();
  return db;
};
const settledYet = (promise: Promise<unknown>) =>
  Promise.race([
    promise.then(
      () => true,
      () => true,
    ),
    Promise.resolve(false),
  ]);

beforeEach(() => {
  jest.useFakeTimers();
  openings = [];
  const open = () => {
    const opening: Opening = {};
    openings.push(opening);
    return opening;
  };
  Object.assign(globalThis, { indexedDB: { open } });
});
afterEach(() => {
  jest.useRealTimers();
  delete (globalThis as { indexedDB?: unknown }).indexedDB;
});

test("an open blocked by another version fails within a second, and later ones wait on the same request", async () => {
  const history = new HistoryRepository();
  const blocked = "History is open elsewhere in another version.";
  const first = history.clears();
  openings[0]!.onblocked!();
  jest.advanceTimersByTime(999);
  expect(await settledYet(first)).toBe(false);
  jest.advanceTimersByTime(1);
  await expect(first).rejects.toThrow(blocked);
  // A second request would wait behind the first without ever hearing it is blocked.
  const second = history.clears();
  expect(openings).toHaveLength(1);
  jest.advanceTimersByTime(1_000);
  await expect(second).rejects.toThrow(blocked);
  const third = history.clears();
  answer(openings[0]!, 4);
  expect(await third).toBe(4);
  expect(openings).toHaveLength(1);
  history.close();
});

test("a late answer is let go, other versions are refused, and without storage nothing waits", async () => {
  const history = new HistoryRepository();
  const late = history.clears();
  jest.advanceTimersByTime(4_999);
  expect(await settledYet(late)).toBe(false);
  jest.advanceTimersByTime(1);
  await expect(late).rejects.toThrow("History storage did not open in time.");
  expect(answer(openings[0]!, 0).closed).toBe(true);

  const refused = history.clears();
  expect(openings).toHaveLength(2);
  const aborted = jest.fn();
  openings[1]!.transaction = { abort: aborted };
  openings[1]!.onupgradeneeded!({ oldVersion: 1 });
  openings[1]!.onerror!();
  expect(aborted).toHaveBeenCalledTimes(1);
  await expect(refused).rejects.toThrow("Unsupported history database version. Saved data has not been changed.");

  delete (globalThis as { indexedDB?: unknown }).indexedDB;
  await expect(history.put(record("a"), 0)).rejects.toBeInstanceOf(HistoryRefusal);
  expect(jest.getTimerCount()).toBe(0);
});

test("an open let go by close() is refused at once when the next one takes its request", async () => {
  const history = new HistoryRepository();
  let refusal: unknown;
  history.clears().catch((error: unknown) => (refusal = error));
  history.close();
  const next = history.clears();
  expect(openings).toHaveLength(1);
  answer(openings[0]!, 2);
  expect(await next).toBe(2);
  expect(String(refusal)).toBe("Error: History storage was closed before it opened.");
  expect(jest.getTimerCount()).toBe(0);
  history.close();
});
