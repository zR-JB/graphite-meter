import { expect, test } from "bun:test";
import { HistoryWriteQueue } from "./writeQueue";
import {
  InvalidHistoryRecordError,
  StaleHistoryGenerationError,
} from "./errors";
import { historyChanges } from "./changes";
import type { HistoryRecord } from "./types";
import { historyRecord } from "./test-helpers.test";

const valid = historyRecord();

test("permanent candidates are dropped while later valid candidates proceed", async () => {
  const saved: string[] = [];
  const dropped: string[] = [];
  const queue = new HistoryWriteQueue(
    async (record) => {
      saved.push(record.id);
    },
    async () => undefined,
    () => undefined,
    (record) => dropped.push(record.id),
    () => undefined,
  );
  queue.enqueue({ ...valid, id: "bad" } as unknown as HistoryRecord);
  queue.enqueue(valid);
  await queue.flush();
  expect(dropped).toEqual(["bad"]);
  expect(saved).toEqual([valid.id]);
});

test("a permanent repository rejection cannot block the next accepted write", async () => {
  const first = historyRecord(16);
  const second = historyRecord(17);
  const saved: string[] = [];
  const dropped: string[] = [];
  const queue = new HistoryWriteQueue(
    async (record) => {
      if (record.id === first.id) throw new InvalidHistoryRecordError();
    },
    async () => undefined,
    (record) => saved.push(record.id),
    (record) => dropped.push(record.id),
    () => undefined,
  );
  queue.enqueue(first);
  queue.enqueue(second);
  await queue.flush();
  expect(dropped).toEqual([first.id]);
  expect(saved).toEqual([second.id]);
});

test("a transient outage retains more than 128 accepted candidates in FIFO order", async () => {
  const candidates = Array.from({ length: 160 }, (_, index) =>
    historyRecord(index + 16),
  );
  const attempts: string[] = [];
  const saved: string[] = [];
  let unavailable = true;
  let warnings = 0;
  const queue = new HistoryWriteQueue(
    async (record) => {
      attempts.push(record.id);
      if (unavailable) {
        unavailable = false;
        throw new Error("temporary outage");
      }
    },
    async () => undefined,
    (record) => saved.push(record.id),
    () => undefined,
    () => {
      warnings++;
    },
  );
  for (const record of candidates) expect(queue.enqueue(record)).toBe(true);

  await queue.flush();
  expect(saved).toEqual([]);
  expect(warnings).toBe(1);
  await queue.flush();
  expect(attempts).toEqual([
    candidates[0].id,
    ...candidates.map(({ id }) => id),
  ]);
  expect(saved).toEqual(candidates.map(({ id }) => id));
  expect(warnings).toBe(1);
});

test("metadata repair resynchronizes queued writes without a false save", async () => {
  const repairGeneration = "repair-00000000-0000-4000-8000-000000000127";
  const records = [historyRecord(16), historyRecord(17)];
  const attempts: Array<{ id: string; generation: string }> = [];
  const saved: string[] = [];
  let repairNeeded = true;
  const queue = new HistoryWriteQueue(
    async (record, _isCurrent, generation) => {
      attempts.push({ id: record.id, generation });
      if (repairNeeded) {
        repairNeeded = false;
        throw new StaleHistoryGenerationError(repairGeneration);
      }
    },
    async () => undefined,
    (record) => saved.push(record.id),
    () => undefined,
    () => undefined,
  );
  records.forEach((record) => queue.enqueue(record));
  await queue.flush();

  expect(attempts).toEqual([
    { id: records[0].id, generation: "" },
    { id: records[0].id, generation: repairGeneration },
    { id: records[1].id, generation: repairGeneration },
  ]);
  expect(saved).toEqual(records.map(({ id }) => id));

  const later = historyRecord(18);
  queue.enqueue(later);
  await queue.flush();
  expect(attempts.at(-1)).toEqual({
    id: later.id,
    generation: repairGeneration,
  });
  expect(saved).toEqual([...records, later].map(({ id }) => id));
});

test("a newer clear wins over a delayed metadata repair response", async () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  let storedGeneration = "";
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: () => storedGeneration || null,
      setItem: (_key: string, value: string) => {
        storedGeneration = value;
      },
      removeItem: () => {
        storedGeneration = "";
      },
    },
  });
  const saved: string[] = [];
  let firstAttempt = true;
  try {
    const queue = new HistoryWriteQueue(
      async () => {
        if (firstAttempt) {
          firstAttempt = false;
          storedGeneration = "clear-00000000-0000-4000-8000-000000000127";
          throw new StaleHistoryGenerationError(
            "repair-00000000-0000-4000-8000-000000000126",
          );
        }
      },
      async () => undefined,
      (record) => saved.push(record.id),
      () => undefined,
      () => undefined,
    );
    queue.enqueue(valid);
    await queue.flush();
    expect(saved).toEqual([]);

    const next = historyRecord(16);
    queue.enqueue(next);
    await queue.flush();
    expect(saved).toEqual([next.id]);
  } finally {
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else Reflect.deleteProperty(globalThis, "localStorage");
  }
});

test("a malformed cross-tab clear message cannot mutate queued work", async () => {
  const original = Object.getOwnPropertyDescriptor(
    globalThis,
    "BroadcastChannel",
  );
  let channel: { onmessage: ((event: MessageEvent) => void) | null } | null =
    null;
  class TestBroadcastChannel {
    onmessage: ((event: MessageEvent) => void) | null = null;
    constructor() {
      channel = this;
    }
    close() {}
  }
  Object.defineProperty(globalThis, "BroadcastChannel", {
    configurable: true,
    value: TestBroadcastChannel,
  });
  let release!: () => void;
  const saved: string[] = [];
  try {
    const queue = new HistoryWriteQueue(
      async () => new Promise<void>((resolve) => (release = resolve)),
      async () => undefined,
      (record) => saved.push(record.id),
      () => undefined,
      () => undefined,
    );
    const stop = historyChanges((change) => {
      if (change.type === "clear") queue.clear(change.generation);
    });
    queue.enqueue(valid);
    await Promise.resolve();
    channel!.onmessage?.({
      data: { type: "clear", generation: "clear-attacker", extra: true },
    } as MessageEvent);
    release();
    await queue.flush();
    stop();
    expect(saved).toEqual([valid.id]);
  } finally {
    if (original)
      Object.defineProperty(globalThis, "BroadcastChannel", original);
    else Reflect.deleteProperty(globalThis, "BroadcastChannel");
  }
});

test("clear generation invalidates in-flight writes in separate tab queues", async () => {
  const other = {
    ...valid,
    id: "00000000-0000-4000-8000-000000000002",
  };
  const writes: string[] = [];
  const saved: string[] = [];
  const removed: string[] = [];
  const releases: Array<() => void> = [];
  const makeQueue = () =>
    new HistoryWriteQueue(
      async (record) => {
        writes.push(record.id);
        await new Promise<void>((resolve) => releases.push(resolve));
      },
      async (id) => {
        removed.push(id);
      },
      (record) => saved.push(record.id),
      () => undefined,
      () => undefined,
    );
  const firstTab = makeQueue();
  const secondTab = makeQueue();
  firstTab.enqueue(valid);
  firstTab.enqueue(historyRecord(3));
  secondTab.enqueue(other);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(writes).toEqual([valid.id, other.id]);
  firstTab.clear("after-clear");
  secondTab.clear("after-clear");
  releases.forEach((release) => release());
  await Promise.all([firstTab.flush(), secondTab.flush()]);
  expect(saved).toEqual([]);
  expect(removed).toEqual([valid.id, other.id]);
});

test("captured empty generation cannot become a post-clear write", async () => {
  const other = {
    ...valid,
    id: "00000000-0000-4000-8000-000000000004",
  };
  let durableGeneration = "";
  const attempted: Array<{ id: string; generation: string }> = [];
  const saved: string[] = [];
  const dropped: string[] = [];
  let warnings = 0;
  const put = async (
    record: HistoryRecord,
    _isCurrent: () => boolean,
    generation: string,
  ) => {
    attempted.push({ id: record.id, generation });
    if (durableGeneration !== generation)
      throw new StaleHistoryGenerationError(durableGeneration);
  };
  const queue = new HistoryWriteQueue(
    put,
    async () => undefined,
    (record) => saved.push(record.id),
    (record) => dropped.push(record.id),
    () => {
      warnings++;
    },
  );
  queue.enqueue(valid);
  durableGeneration = "after-clear";
  await queue.flush();
  expect(attempted).toEqual([{ id: valid.id, generation: "" }]);
  expect(saved).toEqual([]);
  expect(dropped).toEqual([]);
  expect(warnings).toBe(0);

  queue.enqueue(other);
  await queue.flush();
  expect(attempted).toEqual([
    { id: valid.id, generation: "" },
    { id: other.id, generation: "after-clear" },
  ]);
  expect(saved).toEqual([other.id]);
});

test("initial empty generation matches absent or empty durable metadata", async () => {
  const saved: string[] = [];
  let durableGeneration: string | undefined;
  const queue = new HistoryWriteQueue(
    async (_record, _isCurrent, generation) => {
      if ((durableGeneration ?? "") !== generation)
        throw new StaleHistoryGenerationError(durableGeneration ?? "");
    },
    async () => undefined,
    (record) => saved.push(record.id),
    () => undefined,
    () => undefined,
  );
  queue.enqueue(valid);
  await queue.flush();
  durableGeneration = "";
  queue.enqueue({ ...valid, id: "00000000-0000-4000-8000-000000000005" });
  await queue.flush();
  expect(saved).toEqual([valid.id, "00000000-0000-4000-8000-000000000005"]);
});

test("disposing a queue invalidates an in-flight write and suppresses callbacks", async () => {
  const entered = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  const callbacks: string[] = [];
  let current: (() => boolean) | undefined;
  const queue = new HistoryWriteQueue(
    async (_record, isCurrent) => {
      current = isCurrent;
      entered.resolve();
      await release.promise;
    },
    async () => {
      callbacks.push("removed");
    },
    () => {
      callbacks.push("saved");
    },
    () => {
      callbacks.push("permanent");
    },
    () => {
      callbacks.push("transient");
    },
  );
  queue.enqueue(valid);
  await entered.promise;
  expect(current!()).toBe(true);
  queue.dispose();
  expect(current!()).toBe(false);
  expect(queue.enqueue(historyRecord(2))).toBe(false);
  release.resolve();
  await queue.flush();
  expect(callbacks).toEqual([]);
});

test("disposing before a scheduled write prevents repository access", async () => {
  let writes = 0;
  const queue = new HistoryWriteQueue(
    async () => {
      writes++;
    },
    async () => undefined,
    () => undefined,
    () => undefined,
    () => undefined,
  );
  queue.enqueue(valid);
  queue.dispose();
  await queue.flush();
  expect(writes).toBe(0);
});
