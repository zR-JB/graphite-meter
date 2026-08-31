import { expect, test } from "bun:test";
import { HistoryWriteQueue } from "./writeQueue";
import type { HistoryRecordV1 } from "./types";

const valid = {
  schemaVersion: 1,
  id: "00000000-0000-4000-8000-000000000001",
  startedAt: 1,
  completedAt: 2,
  durationMs: 1,
  stages: {
    latency: {
      status: "not-run",
      result: null,
      lanes: {
        latency: null,
        download: null,
        upload: null,
        bidirectional: null,
      },
    },
    download: { status: "not-run", result: null },
    upload: { status: "not-run", result: null },
    bidirectional: { status: "not-run", down: null, up: null },
  },
  bufferbloat: null,
  totalBytes: 0,
  server: { name: "s", location: null, engine: "e" },
  transport: {
    throughput: { protocol: null, kind: null },
    latency: { protocol: null, kind: null },
  },
  ipVersion: null,
  client: { build: "b" },
  failures: [],
  wireEstimates: null,
} satisfies HistoryRecordV1;

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
  queue.enqueue({ ...valid, id: "bad" } as unknown as HistoryRecordV1);
  queue.enqueue(valid);
  await queue.flush();
  expect(dropped).toEqual(["bad"]);
  expect(saved).toEqual([valid.id]);
});

test("transient failures leave the candidate queued for retry", async () => {
  let attempts = 0;
  let transient = 0;
  const queue = new HistoryWriteQueue(
    async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("temporary");
    },
    async () => undefined,
    () => undefined,
    () => undefined,
    () => {
      transient += 1;
    },
  );
  queue.enqueue(valid);
  await queue.flush();
  expect(attempts).toBe(1);
  expect(transient).toBe(1);
  await queue.flush();
  expect(attempts).toBe(2);
  expect(transient).toBe(1);
});

test("clear generation drops queued work and cleans an in-flight stale write", async () => {
  let release!: () => void;
  const writes: string[] = [];
  const removed: string[] = [];
  const queue = new HistoryWriteQueue(
    async (record) => {
      writes.push(record.id);
      await new Promise<void>((resolve) => (release = resolve));
    },
    async (id) => {
      removed.push(id);
    },
    () => undefined,
    () => undefined,
    () => undefined,
  );
  queue.enqueue(valid);
  await Promise.resolve();
  queue.clear("after-clear");
  release();
  await queue.flush();
  expect(writes).toEqual([valid.id]);
  expect(removed).toEqual([valid.id]);
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
