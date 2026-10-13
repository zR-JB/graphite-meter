import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { signal } from "../../lib/signal";
import type { HistoryRecord } from "./records";
import { HistoryRefusal, HistoryRepository, type HistoryChange } from "./repository";
import { createHistorySaving } from "./saving";

const RETRYING = "Unable to save this result locally. Future writes will be retried.";
const record = (id: string) => ({ id }) as HistoryRecord;
/** Lets every promise already settled run on. */
const turns = async () => {
  for (let i = 0; i < 20; i++) await null;
};

/** History in memory; each write waits until the test answers it. */
function memoryHistory() {
  const rows = new Map<string, HistoryRecord>();
  const listeners = new Set<(change: HistoryChange) => void>();
  const writes: { id: string; clears: number; answer(failure?: Error): void }[] = [];
  let clears = 0;
  const failing = { clears: false };
  const notify = (change: HistoryChange) => {
    for (const listener of listeners) listener(change);
  };
  const repository: Pick<HistoryRepository, "clears" | "put" | "inspect" | "onChange"> = {
    clears: async () => {
      if (failing.clears) throw new Error("busy");
      return clears;
    },
    put: (saved, queued) =>
      new Promise((resolve, reject) => {
        const answer = (failure?: Error) => {
          if (failure) return reject(failure);
          if (clears > queued) return resolve(false);
          rows.set(saved.id, saved);
          resolve(true);
        };
        writes.push({ id: saved.id, clears: queued, answer });
      }),
    inspect: async (id) => {
      const saved = rows.get(id);
      return saved ? { status: "ready", record: saved } : { status: "missing" };
    },
    onChange(listener) {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
  };
  return {
    repository,
    rows,
    writes,
    listeners,
    failing,
    clear() {
      rows.clear();
      clears++;
      notify({ type: "cleared" });
    },
    delete(id: string) {
      rows.delete(id);
      notify({ type: "deleted", ids: [id] });
    },
    /** Another tab changes History. */
    elsewhere(change: () => void) {
      change();
      notify({ type: "elsewhere" });
    },
  };
}

function windowEvents() {
  const listeners = new Map<string, Set<() => void>>();
  const target = {
    addEventListener: (type: string, listener: () => void) =>
      listeners.set(type, (listeners.get(type) ?? new Set()).add(listener)),
    removeEventListener: (type: string, listener: () => void) => listeners.get(type)?.delete(listener),
    dispatch: (type: string) => listeners.get(type)?.forEach((listener) => listener()),
    count: () => [...listeners.values()].reduce((count, set) => count + set.size, 0),
  };
  return target as typeof target & Pick<Window, "addEventListener" | "removeEventListener">;
}

beforeEach(() => void jest.useFakeTimers());
afterEach(() => void jest.useRealTimers());

test("writes go one at a time and in order, each under the clear count read when it was queued", async () => {
  const history = memoryHistory();
  const saver = createHistorySaving(history.repository, signal(true), windowEvents());
  saver.offer(record("a"));
  saver.offer(record("b"));
  await turns();
  expect(history.writes.map(({ id }) => id)).toEqual(["a"]);
  // Both were queued before this clear, in this tab or any other: neither may bring History back.
  history.clear();
  saver.offer(record("c"));
  for (const write of [0, 1, 2]) {
    expect(saver.pending()).toBe(true);
    history.writes[write]!.answer();
    await turns();
  }
  expect(history.writes.map(({ id, clears }) => [id, clears])).toEqual([
    ["a", 0],
    ["b", 0],
    ["c", 1],
  ]);
  expect([...history.rows.keys()]).toEqual(["c"]);
  expect([saver.pending(), saver.unsaved(), saver.warning()]).toEqual([false, false, ""]);
});

test("other failures retry every 15 s and as the page returns until saved; a refusal is final", async () => {
  const history = memoryHistory();
  const saving = signal(true);
  const events = windowEvents();
  const saver = createHistorySaving(history.repository, saving, events);
  saver.offer(record("a"));
  await turns();
  history.writes[0]!.answer(new Error("busy"));
  await turns();
  expect([saver.pending(), saver.unsaved(), saver.warning()]).toEqual([true, true, RETRYING]);
  expect([events.count(), jest.getTimerCount()]).toEqual([3, 1]);
  jest.advanceTimersByTime(15_000);
  await turns();
  history.writes[1]!.answer(new Error("busy"));
  await turns();
  for (const type of ["focus", "online", "visibilitychange"]) {
    events.dispatch(type);
    await turns();
  }
  // Retries never overlap the write in flight.
  expect(history.writes).toHaveLength(3);
  history.writes[2]!.answer();
  await turns();
  expect([saver.pending(), saver.unsaved(), saver.warning()]).toEqual([false, false, ""]);
  expect([events.count(), jest.getTimerCount()]).toEqual([0, 0]);

  saver.offer(record("b"));
  await turns();
  history.writes[3]!.answer(new HistoryRefusal("Browser storage is full. This result was not saved."));
  await turns();
  expect([saver.pending(), saver.unsaved(), saver.warning()]).toEqual([
    false,
    true,
    "Browser storage is full. This result was not saved.",
  ]);
  saving.set(false);
  saving.set(true);
  await turns();
  expect([history.writes.length, events.count(), jest.getTimerCount()]).toEqual([4, 0, 0]);
});

test("dispose lets go of every timer and listener, and a write landing after it settles nothing", async () => {
  const history = memoryHistory();
  const events = windowEvents();
  const saver = createHistorySaving(history.repository, signal(true), events);
  saver.offer(record("a"));
  await turns();
  history.writes[0]!.answer(new Error("busy"));
  await turns();
  events.dispatch("focus");
  await turns();
  expect([events.count(), jest.getTimerCount(), history.listeners.size]).toEqual([3, 1, 1]);
  saver.dispose();
  expect([events.count(), jest.getTimerCount(), history.listeners.size]).toEqual([0, 0, 0]);
  history.writes[1]!.answer();
  await turns();
  expect([saver.pending(), saver.warning()]).toEqual([true, RETRYING]);
});

test("unsaved holds exactly while the offered result is not in History", async () => {
  const history = memoryHistory();
  const saving = signal(false);
  const saver = createHistorySaving(history.repository, saving, windowEvents());
  const saved = async (id: string) => {
    saver.offer(record(id));
    await turns();
    history.writes.at(-1)!.answer();
    await turns();
    return saver.unsaved();
  };

  saver.offer(record("a"));
  expect(saver.unsaved()).toBe(true);
  saving.set(true);
  await turns();
  expect(saver.unsaved()).toBe(true);
  history.writes[0]!.answer();
  await turns();
  expect(saver.unsaved()).toBe(false);
  saver.offer(record("a"));
  expect(saver.unsaved()).toBe(false);
  history.delete("a");
  expect(saver.unsaved()).toBe(true);
  // A result deleted from History is never saved again.
  saving.set(false);
  saving.set(true);
  await turns();
  expect(history.writes).toHaveLength(1);

  expect(await saved("b")).toBe(false);
  history.clear();
  expect(saver.unsaved()).toBe(true);

  expect(await saved("c")).toBe(false);
  history.elsewhere(() => history.rows.set("x", record("x")));
  await turns();
  expect(saver.unsaved()).toBe(false);
  history.elsewhere(() => history.rows.delete("c"));
  await turns();
  expect(saver.unsaved()).toBe(true);

  saver.offer(record("d"));
  await turns();
  history.clear();
  history.writes.at(-1)!.answer();
  await turns();
  expect([history.rows.has("d"), saver.unsaved(), saver.warning()]).toEqual([false, true, ""]);
  saver.withdraw();
  expect(saver.unsaved()).toBe(false);
});

test("without storage for History a write is refused at once, so signing out never waits for it", async () => {
  const events = windowEvents();
  const history = new HistoryRepository();
  const saver = createHistorySaving(history, signal(true), events);
  saver.offer(record("a"));
  await turns();
  expect([saver.pending(), saver.unsaved(), saver.warning()]).toEqual([
    false,
    true,
    "This browser has no storage for History: results are not saved.",
  ]);
  expect([events.count(), jest.getTimerCount()]).toEqual([0, 0]);
  saver.dispose();
});

test("while storage keeps failing, results wait in a bounded queue and the oldest waiting ones are given up", async () => {
  const history = memoryHistory();
  const saver = createHistorySaving(history.repository, signal(true), windowEvents());
  for (let i = 0; i < 20; i++) saver.offer(record(String(i)));
  await turns();
  history.writes[0]!.answer(new Error("busy"));
  await turns();
  jest.advanceTimersByTime(15_000);
  await turns();
  // Storage recovered: the head, then what still waits, in order.
  for (let write = 1; write < history.writes.length; write++) {
    history.writes[write]!.answer();
    await turns();
  }
  expect([...history.rows.keys()]).toEqual(["0", "13", "14", "15", "16", "17", "18", "19"]);
  expect([saver.pending(), saver.unsaved()]).toEqual([false, false]);
});

test("a write whose clear count could not be read never comes back after a clear", async () => {
  // Nothing in this tab has read the count yet: the write has no count it can trust.
  const history = memoryHistory();
  history.failing.clears = true;
  const saver = createHistorySaving(history.repository, signal(true), windowEvents());
  saver.offer(record("before-clear"));
  await turns();
  history.clear();
  history.failing.clears = false;
  await turns();
  expect(history.writes).toEqual([]);
  expect([...history.rows.keys()]).toEqual([]);
  expect([saver.pending(), saver.unsaved()]).toEqual([false, true]);
  expect(saver.warning()).toContain("could not be read");
});

test("a write whose clear count could not be read takes the count this tab knows to be current", async () => {
  const history = memoryHistory();
  const saver = createHistorySaving(history.repository, signal(true), windowEvents());
  await turns();
  history.failing.clears = true;
  saver.offer(record("a"));
  await turns();
  history.writes[0]!.answer();
  await turns();
  expect(history.writes.map(({ id, clears }) => [id, clears])).toEqual([["a", 0]]);
  expect([...history.rows.keys()]).toEqual(["a"]);
  // A change in another tab may have cleared History: until the count is read again, a write cannot be ordered.
  history.elsewhere(() => history.clear());
  saver.offer(record("b"));
  await turns();
  expect(history.writes.map(({ id }) => id)).toEqual(["a"]);
  expect(saver.unsaved()).toBe(true);
});
