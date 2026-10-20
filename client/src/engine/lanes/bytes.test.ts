import { expect, test } from "bun:test";
import { ByteReports, largestPost, partBytes } from "./bytes";

const MiB = 1024 * 1024;

test("a server's reporters together report about every 10 ms, whatever their number", () => {
  for (const [reporters, expected] of [
    [1, [10, 20, 30, 40]],
    [4, [40]],
  ] as const) {
    const reported: number[] = [];
    const reports = new ByteReports(() => reported.push(at));
    let at = 0;
    reports.share(reporters);
    reports.flush(0);
    for (at = 1; at <= 45; at++) reports.count(1, at);
    expect(reported).toEqual([...expected]);
  }
});

test("a flush reads the epoch's total as counted by its cutoff, from readings kept once a millisecond", () => {
  const reports = new ByteReports(() => {});
  reports.measure(1);
  reports.flush(0);
  for (let at = 0; at < 100; at += 0.25) reports.count(1, at);
  expect(reports.totalAt(-1)).toEqual({ total: 0, at: -1 });
  // The reading at 50 ms counted the bytes at 0 ms through 50 ms. The ones until 50.75 ms come with the next.
  expect(reports.totalAt(50.75)).toEqual({ total: 201, at: 50.75 });
  expect(reports.totalAt(1_000)).toEqual({ total: 400, at: 1_000 });
  // A lane that reopens in its epoch counts on.
  reports.measure(1);
  reports.count(100, 100);
  expect(reports.totalAt(100).total).toBe(500);
  // Past the readings kept, the earliest kept one answers with its own time.
  for (let at = 101; at < 700; at++) reports.count(1, at);
  expect(reports.totalAt(10)).toEqual({ total: 588, at: 188 });
});

test("a new epoch counts the bytes read since its time, though the lane hears of it later", () => {
  const reported: [number, number][] = [];
  const reports = new ByteReports((bytes, epoch) => reported.push([bytes, epoch]));
  reports.flush(0);
  for (let at = 0; at < 100; at++) reports.count(1, at);
  reports.measure(1, 59.5);
  expect(reports.totalAt(80).total).toBe(21);
  expect(reports.totalAt(100).total).toBe(40);
  reports.flush(100);
  expect(reported.filter(([, epoch]) => epoch === 1)).toEqual([[40, 1]]);
});

test("a cutoff counts the bytes read by then, each at its own clock reading, whenever it is asked", () => {
  const reports = new ByteReports(() => {});
  reports.measure(1);
  reports.flush(0);
  reports.count(1_000, 1_000);
  reports.count(100, 1_001);
  expect(reports.totalAt(1_050)).toEqual({ total: 1_100, at: 1_050 });
  // Bytes read after the cutoff stay out of it, however soon after.
  reports.count(100, 1_080);
  expect(reports.totalAt(1_050)).toEqual({ total: 1_100, at: 1_050 });
  expect(reports.totalAt(1_080)).toEqual({ total: 1_200, at: 1_080 });
});

test("a lane's largest POST is its device-memory share of its server's lanes, Firefox's of the run's", () => {
  for (const [lanes, sharing, deviceMemory, expected] of [
    [1, 3, 8, 256 * MiB],
    [4, 4, 8, 64 * MiB],
    [4, 4, 2, 4 * MiB],
    [4, 4, 4, 6 * MiB],
    [16, 16, 2, 2 * MiB],
    [0, 0, 8, 256 * MiB],
    [1, 1, undefined, 128 * MiB],
    [4, 4, undefined, 32 * MiB],
    // Three servers of one lane each share Firefox's budget.
    [1, 3, undefined, 42 * MiB + Math.floor((2 * MiB) / 3)],
    [128, 128, undefined, 2 * MiB],
  ] as const)
    expect(
      largestPost(lanes, sharing, deviceMemory),
      `${lanes} of ${sharing} lanes, ${deviceMemory} GB`,
    ).toBe(expected);
});

test("no POST spans more than eight parts, which Firefox would copy through a pipe", () => {
  for (const largest of [2 * MiB, 32 * MiB, 33 * MiB, 64 * MiB, 128 * MiB, 256 * MiB]) {
    const part = partBytes(largest);
    expect(Math.ceil(largest / part), `${largest / MiB} MiB POSTs`).toBeLessThanOrEqual(8);
    expect(part % (4 * MiB)).toBe(0);
  }
});
