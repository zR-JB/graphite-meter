import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { IDBFactory } from "fake-indexeddb";
import { HISTORY_LIMIT, type HistoryRecord } from "./records";
import { HistoryRefusal, HistoryRepository } from "./repository";

const notRun = { latency: null, download: null, upload: null, bidirectional: null };
const record = (id: string) =>
  ({
    schemaVersion: 6,
    id,
    completedAt: 0,
    build: "v1",
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
      stages: {
        latency: "not-run",
        download: "not-run",
        upload: "not-run",
        bidirectional: "not-run",
      },
      startedAt: 0,
      durationMs: 0,
    },
  }) satisfies HistoryRecord;

/** Real IndexedDB semantics in memory, holding `rows` at `version` as an earlier build left them. */
async function storage(version?: number, rows: unknown[] = []) {
  jest.useRealTimers();
  const factory = new IDBFactory();
  Object.assign(globalThis, { indexedDB: factory });
  if (version !== undefined)
    await new Promise<void>((resolve, reject) => {
      const opening = factory.open("graphite-meter", version);
      opening.onupgradeneeded = () => {
        const db = opening.result;
        db.createObjectStore("results", { keyPath: "id" }).createIndex(
          "completedAt",
          "completedAt",
        );
        db.createObjectStore("meta", { keyPath: "key" });
        for (const row of rows) opening.transaction!.objectStore("results").put(row);
      };
      opening.onsuccess = () => {
        opening.result.close();
        resolve();
      };
      opening.onerror = () => reject(opening.error);
    });
  /** The database's version and its rows as stored, by id. */
  return () =>
    new Promise<{ version: number; rows: unknown[] }>((resolve, reject) => {
      const opening = factory.open("graphite-meter");
      opening.onsuccess = () => {
        const db = opening.result;
        const all = db.transaction("results").objectStore("results").getAll();
        all.onsuccess = () => {
          db.close();
          resolve({ version: db.version, rows: all.result });
        };
      };
      opening.onerror = () => reject(opening.error);
    });
}

test("a write lands under the clear count it was queued with; past the limit the oldest readable rows go", async () => {
  const unreadable = { id: "junk", completedAt: -1 };
  const rows = [
    unreadable,
    ...Array.from({ length: HISTORY_LIMIT }, (_, i) => ({ ...record(`r${i}`), completedAt: i })),
  ];
  const stored = await storage(3, rows);
  const history = new HistoryRepository();
  const changes: unknown[] = [];
  history.onChange((change) => changes.push(change));
  expect(await history.clears()).toBe(0);
  expect(await history.put({ ...record("new"), completedAt: HISTORY_LIMIT }, 0)).toBe(true);
  // A clear in any tab drops a write queued before it.
  await history.clear();
  expect(await history.put(record("late"), 0)).toBe(false);
  expect(await history.put({ ...record("after"), completedAt: 5 }, 1)).toBe(true);
  expect(changes).toEqual([
    { type: "deleted", ids: ["r0"] },
    { type: "saved", record: { ...record("new"), completedAt: HISTORY_LIMIT } },
    { type: "cleared" },
    { type: "saved", record: { ...record("after"), completedAt: 5 } },
  ]);
  expect((await stored()).rows.map((row) => (row as HistoryRecord).id)).toEqual(["after"]);
  history.close();
});

test("version 2 and 3 rows are upgraded once, in place; unreadable ones stay as stored and other versions are refused", async () => {
  const { result } = record("older");
  const older = {
    ...record("older"),
    schemaVersion: 5,
    engine: "e",
    result: { ...result, latency: { reportedMs: 9, jitterMs: null } },
  };
  const unreadable = { id: "junk", schemaVersion: 7 };
  for (const version of [2, 3]) {
    const stored = await storage(version, [older, unreadable]);
    const history = new HistoryRepository();
    const listed = await history.list(new AbortController().signal, (each) => each);
    expect(listed).toEqual({
      records: [
        {
          ...record("older"),
          result: { ...result, latency: { reportedMs: 9, jitterMs: null, stabilityPct: null } },
        },
      ],
      malformedCount: 1,
    });
    history.close();
    expect(await stored()).toEqual({ version: 4, rows: [unreadable, listed.records[0]] });
  }

  const refused = await storage(1, [unreadable]);
  const other = new HistoryRepository();
  await expect(other.clears()).rejects.toThrow(
    "Unsupported History database version. Saved data has not been changed.",
  );
  other.close();
  expect(await refused()).toEqual({ version: 1, rows: [unreadable] });
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
  await expect(refused).rejects.toThrow(
    "Unsupported History database version. Saved data has not been changed.",
  );

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
