import { expect, test } from "bun:test";
import type { HistoryRecord } from "../../app/history/records";
import { HistoryRefusal, type HistoryChange, type HistoryRepository } from "../../app/history/repository";
import { scope, signal } from "../../lib/signal";
import { createArchive } from "./archive";

const notRun = { latency: null, download: null, upload: null, bidirectional: null };
const record = (id: string, completedAt: number): HistoryRecord => ({
  schemaVersion: 5,
  id,
  completedAt,
  build: "v1",
  engine: "e",
  result: {
    ...notRun,
    latencyByStage: notRun,
    addedLatency: null,
    multiServer: {
      selection: [{ id: "a", name: "Edge", url: "" }],
      participants: ["a"],
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
});

/** A repository whose reads the test answers one at a time. */
function repository() {
  const reads: PromiseWithResolvers<unknown>[] = [];
  const read = () => {
    const pending = Promise.withResolvers<unknown>();
    reads.push(pending);
    return pending.promise;
  };
  let listener: (change: HistoryChange) => void = () => {};
  const inspected: string[] = [];
  const fake = {
    list: async (_: AbortSignal, keep: (record: HistoryRecord) => unknown) => {
      const { records, malformedCount } = (await read()) as { records: HistoryRecord[]; malformedCount: number };
      return { records: records.map(keep), malformedCount };
    },
    inspect: (id: string) => {
      inspected.push(id);
      return read();
    },
    onChange: (next: (change: HistoryChange) => void) => {
      listener = next;
      return () => {};
    },
  } as unknown as HistoryRepository;
  return { fake, reads, inspected, change: (change: HistoryChange) => listener(change) };
}
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test("a failed quiet reload or lookup keeps the list on screen and says why", async () => {
  const { fake, reads, change } = repository();
  const selectedId = signal<string | null>(null);
  const [archive, dispose] = scope(() => createArchive(fake, selectedId));
  reads[0]!.resolve({ records: [record("a", 2)], malformedCount: 0 });
  await settle();
  expect(archive.state()).toBe("ready");

  // Another tab's change.
  change({ type: "elsewhere" });
  reads[1]!.reject(new Error("closed"));
  await settle();
  expect(archive.state()).toBe("ready");
  expect(archive.records()).toHaveLength(1);
  expect(archive.error()).toBe("History could not be read again, so this list may be out of date.");

  selectedId.set("b");
  reads[2]!.reject(new Error("closed"));
  await settle();
  expect(archive.state()).toBe("ready");
  expect(archive.records()).toHaveLength(1);
  expect(archive.error()).toBe("This result could not be read from History.");
  expect(archive.selected().status).toBe("pending");
  dispose();
});

test("a reload History refuses for good, such as a newer version's database, leaves the list", async () => {
  const { fake, reads, change } = repository();
  const [archive, dispose] = scope(() => createArchive(fake, signal<string | null>(null)));
  reads[0]!.resolve({ records: [record("a", 2)], malformedCount: 0 });
  await settle();
  change({ type: "elsewhere" });
  reads[1]!.reject(new HistoryRefusal("Unsupported history database version. Saved data has not been changed."));
  await settle();
  expect(archive.state()).toBe("error");
  dispose();
});

test("the list keeps entries only; the open result alone is read whole, again as it leaves the list", async () => {
  const { fake, reads, inspected, change } = repository();
  const selectedId = signal<string | null>(null);
  const [archive, dispose] = scope(() => createArchive(fake, selectedId));
  const [a, b] = [record("a", 2), record("b", 1)];
  reads[0]!.resolve({ records: [a, b], malformedCount: 0 });
  await settle();
  expect(archive.records().map(({ id }) => id)).toEqual(["a", "b"]);
  expect(archive.records().some((entry) => "result" in entry || "record" in entry)).toBe(false);

  selectedId.set("a");
  expect(archive.selected().status).toBe("pending");
  expect(inspected).toEqual(["a"]);
  reads[1]!.resolve({ status: "ready", record: a });
  await settle();
  expect(archive.selected()).toEqual({ status: "ready", record: a });

  // Deleted in another tab: the list read again no longer holds it.
  change({ type: "elsewhere" });
  reads[2]!.resolve({ records: [b], malformedCount: 0 });
  await settle();
  expect(inspected).toEqual(["a", "a"]);
  reads[3]!.resolve({ status: "missing" });
  await settle();
  expect(archive.selected()).toEqual({ status: "missing" });
  dispose();
});
