import { expect, test } from "bun:test";
import { json } from "../../protocol/pins.testutil";
import { Aggregate, type ThroughputResult } from "./aggregate";
import { stabilityPct } from "./stability";

const reported = (result: ThroughputResult | null) =>
  result && { bytesPerSec: result.reportedBytesPerSec, peakBytesPerSec: result.peakBytesPerSec };

test("the coordinated throughput the terminal client shares", async () => {
  for (const vector of await json("aggregation.testvectors.json")) {
    const aggregate = new Aggregate();
    aggregate.begin(vector.stage, vector.participants, vector.boundaries[0].atMs);
    let live: string[] = vector.participants;
    const downloaded = new Map<string, number>();
    for (const boundary of vector.boundaries) {
      if (boundary.dropout) {
        live = live.filter((id) => !boundary.dropout.includes(id));
        aggregate.dropout(live, boundary.atMs);
      }
      const up = Object.fromEntries(
        Object.entries(boundary.up).map(([id, r]: [string, any]) => [
          id,
          r && { ...r, receivedAtMs: boundary.atMs },
        ]),
      );
      aggregate.observe({ atMs: boundary.atMs, down: boundary.down, up }, boundary.final);
      for (const [id, seen] of Object.entries<any>(boundary.observedUp ?? {}))
        aggregate.credit(id, { id: seen.id, bytes: seen.maximum });
      // The browser credits downloads as lanes report them. The vectors credit a counter's new high-water mark.
      for (const [id, bytes] of Object.entries<number>(boundary.down)) {
        const high = downloaded.get(id) ?? bytes;
        aggregate.addDownload(vector.stage, id, bytes - high);
        downloaded.set(id, Math.max(high, bytes));
      }
    }
    const totals = { down: 0, up: 0 };
    for (const id of vector.participants) {
      const each = aggregate.totals(id);
      totals.down += each.down;
      totals.up += each.up;
    }
    expect(totals, vector.name).toEqual(vector.totalBytes);
    const intervals = aggregate.intervals.map((interval) => ({
      reason: interval.reason,
      complete: interval.complete,
      window: interval.full && {
        startMs: interval.full.startMs,
        endMs: interval.full.endMs,
        downBytesPerSec: interval.full.downBytesPerSec,
        upBytesPerSec: interval.full.upBytesPerSec,
      },
    }));
    expect(intervals, vector.name).toEqual(vector.intervals);
    const { down, up } = aggregate.settle(vector.stage, false);
    expect(
      { down: reported(down?.result ?? null), up: reported(up?.result ?? null) },
      vector.name,
    ).toEqual(vector.result);
    for (const [id, expected] of Object.entries<any>(vector.servers ?? {}))
      expect(
        {
          down: reported(aggregate.serverResult(vector.stage, "down", id)),
          up: reported(aggregate.serverResult(vector.stage, "up", id)),
        },
        `${vector.name}: ${id}`,
      ).toEqual(expected);
  }
});

test("a steady window too short on one direction's clock gives way to the full window for both", () => {
  const aggregate = new Aggregate();
  aggregate.begin("bidirectional", ["a"], 0);
  const at = (atMs: number, down: number, up: number, receiverMs: number) =>
    aggregate.observe({
      atMs,
      down: { a: down },
      up: { a: { id: "u", bytes: up, nanos: receiverMs * 1e6 + 1, receivedAtMs: atMs } },
    });
  at(0, 0, 0, 0);
  at(1_000, 1_000, 1_000, 1_000);
  aggregate.trackStable(0.9);
  // From the steady boundary on, the receiver's clock runs only 500 ms, which is too short to stand alone.
  at(2_000, 3_000, 1_500, 1_500);
  const { down, up } = aggregate.settle("bidirectional", true);
  expect([down?.result.reportedBytesPerSec, up?.result.reportedBytesPerSec]).toEqual([
    1_500, 1_000,
  ]);
  expect(down?.window).toBe(up?.window);
  expect(aggregate.intervals[0]!.headline).toBe(down!.window);
});

test("a stage's coverage counts from its first interval after the interval limit let it go", () => {
  const aggregate = new Aggregate();
  aggregate.begin("download", ["a"], 0);
  for (let at = 10; at <= 1_300; at += 10)
    aggregate.begin("download", ["a"], at, "evidence-resumed");
  expect(aggregate.omittedIntervals).toBeGreaterThan(0);
  expect(aggregate.coveredMs("download")).toBe(1_300);
});

test("a stage's stability is of its 250 ms rates however long it runs", () => {
  // A link alternating between nothing and twice its mean every 250 ms shows no stability, before and after buckets merge.
  for (const buckets of [4_094, 4_096, 8_192]) {
    const aggregate = new Aggregate();
    aggregate.begin("download", ["a"], 0);
    aggregate.observe({ atMs: 0, down: { a: 0 }, up: {} });
    let bytes = 0;
    for (let i = 0; i < buckets; i++) {
      const add = i % 2 ? 500 : 0;
      bytes += add;
      aggregate.addDownload("download", "a", add);
      aggregate.observe({ atMs: (i + 1) * 250, down: { a: bytes }, up: {} });
    }
    const result = aggregate.settle("download", false).down!.result;
    expect([buckets, Math.round(result.stabilityPct)]).toEqual([buckets, 0]);
  }
});

test("a departed server keeps its result after the interval limit lets its interval go", () => {
  const aggregate = new Aggregate();
  aggregate.begin("download", ["a", "b"], 0);
  aggregate.observe({ atMs: 0, down: { a: 0, b: 0 }, up: {} });
  aggregate.observe({ atMs: 1_000, down: { a: 1_000, b: 1_000 }, up: {} });
  aggregate.addDownload("download", "a", 1_000);
  aggregate.dropout(["b"], 1_000);
  const before = aggregate.serverResult("download", "down", "a");
  // The survivor goes on through more resumed intervals than the log keeps.
  for (let i = 1; i <= 130; i++) {
    aggregate.begin("download", ["b"], i * 1_000, "evidence-resumed");
    aggregate.observe({ atMs: i * 1_000, down: { b: i * 1_000 }, up: {} });
    aggregate.observe({ atMs: (i + 1) * 1_000, down: { b: (i + 1) * 1_000 }, up: {} });
  }
  expect(aggregate.omittedIntervals).toBeGreaterThan(0);
  expect(before?.reportedBytesPerSec).toBe(1_000);
  expect(aggregate.serverResult("download", "down", "a")).toEqual(before);
  expect(aggregate.serverResult("download", "down", "b")?.reportedBytesPerSec).toBe(1_000);
});

test("a stage keeps its result when every interval the log still holds measured too little", () => {
  const aggregate = new Aggregate();
  aggregate.begin("download", ["a"], 0);
  aggregate.observe({ atMs: 0, down: { a: 0 }, up: {} });
  aggregate.observe({ atMs: 2_000, down: { a: 2_000 }, up: {} });
  // Only resumed intervals too short to report follow, and more of them than the log keeps.
  for (let i = 0; i < 130; i++) {
    const at = 2_000 + i * 100;
    aggregate.begin("download", ["a"], at, "evidence-resumed");
    aggregate.observe({ atMs: at, down: { a: 2_000 + i * 100 }, up: {} });
    aggregate.observe({ atMs: at + 100, down: { a: 2_000 + (i + 1) * 100 }, up: {} });
  }
  expect(aggregate.omittedIntervals).toBeGreaterThan(0);
  expect(aggregate.settle("download", false).down?.result.reportedBytesPerSec).toBe(1_000);
});

test("a steady stretch after a burst reads the stability of its own rates", () => {
  // One very fast bucket precedes a tail 0.05% either side of its mean, so the steady window is the tail alone.
  for (const burst of [1e8, 1e9, 8e9])
    for (const low of [4, 4_000, 40_000]) {
      const rates = [
        burst,
        ...Array.from({ length: 40 }, (_, i) => low + (i % 2 ? low * 0.001 : 0)),
      ];
      const aggregate = new Aggregate();
      aggregate.begin("download", ["a"], 0);
      aggregate.observe({ atMs: 0, down: { a: 0 }, up: {} });
      let total = 0;
      rates.forEach((rate, i) => {
        total += rate * 0.25;
        aggregate.addDownload("download", "a", rate * 0.25);
        aggregate.observe({ atMs: (i + 1) * 250, down: { a: total }, up: {} });
        aggregate.trackStable(aggregate.confidence().score);
      });
      const headline = aggregate.settle("download", true).down!;
      const from = headline.window.startMs / 250;
      expect(from).toBeGreaterThan(0);
      expect(headline.result.stabilityPct).toBeCloseTo(stabilityPct(rates.slice(from)), 5);
    }
});

test("a final boundary in the last tick's time closes the window with what the lanes had counted, never a stale receiver", () => {
  const down = new Aggregate();
  down.begin("download", ["a"], 0);
  down.observe({ atMs: 0, down: { a: 0 }, up: {} });
  down.observe({ atMs: 1_000, down: { a: 1_000 }, up: {} });
  down.observe({ atMs: 1_000, down: { a: 1_040 }, up: {} }, true);
  expect(down.intervals.map(({ complete, full }) => [complete, full?.downBytesPerSec])).toEqual([
    [true, 1_040],
  ]);

  const empty = new Aggregate();
  empty.begin("download", ["a"], 0);
  empty.observe({ atMs: 0, down: { a: 0 }, up: {} });
  empty.observe({ atMs: 0, down: { a: 40 }, up: {} }, true);
  expect(empty.intervals.map(({ complete, full }) => [complete, full])).toEqual([[true, null]]);

  const up = new Aggregate();
  const at = (bytes: number, nanos: number) => ({
    up: { a: { id: "r", bytes, nanos, receivedAtMs: 0 } },
    down: {},
  });
  up.begin("upload", ["a"], 0);
  up.observe({ atMs: 0, ...at(0, 1) });
  up.observe({ atMs: 1_000, ...at(1_000, 1e9 + 1) });
  up.observe({ atMs: 1_000, ...at(1_500, 1e9 + 1) }, true);
  expect(up.intervals[0]!.full?.upBytesPerSec).toBe(1_000);
});
