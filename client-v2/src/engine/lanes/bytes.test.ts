import { expect, test } from "bun:test";
import { ByteReports, largestPost } from "./bytes";

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
  // The reading at 50 ms counted the bytes at 0 ms through 50 ms; the ones until 50.75 ms come with the next.
  expect(reports.totalAt(50.75)).toEqual({ total: 201, at: 50.75 });
  expect(reports.totalAt(1_000)).toEqual({ total: 400, at: 1_000 });
  // A lane reopening in its epoch counts on; a new epoch starts from nothing.
  reports.measure(1);
  reports.count(100, 100);
  expect(reports.totalAt(100).total).toBe(500);
  reports.measure(2);
  expect(reports.totalAt(100).total).toBe(0);
  // Past the readings kept, the earliest kept one answers with its own time.
  for (let at = 0; at < 600; at++) reports.count(1, at);
  expect(reports.totalAt(10)).toEqual({ total: 89, at: 88 });
});

test("bytes counted without a clock reading reach a cutoff by its share of the time they could have arrived in", () => {
  const reports = new ByteReports(() => {});
  reports.measure(1);
  reports.count(1_000, 1_000);
  // Sixteen datagrams' worth arrived after the count at 1,000 ms, by the time the flush asks at 1,010 ms.
  for (let i = 0; i < 16; i++) reports.add(100);
  expect(reports.totalAt(1_010, 1_010).total).toBe(2_600);
  expect(reports.totalAt(1_005, 1_010).total).toBe(1_800);
  expect(reports.totalAt(1_000, 1_010).total).toBe(1_000);
});

test("a lane's largest POST is v1's device-memory share divided across the lanes", () => {
  for (const [lanes, deviceMemory, expected] of [
    [1, 8, 256 * MiB],
    [4, 8, 64 * MiB],
    [4, 2, 4 * MiB],
    [4, 4, 6 * MiB],
    [16, 2, 2 * MiB],
    [0, 8, 256 * MiB],
    [1, undefined, 128 * MiB],
    [4, undefined, 32 * MiB],
    [128, undefined, 2 * MiB],
  ] as const)
    expect(largestPost(lanes, deviceMemory), `${lanes} lanes, ${deviceMemory} GB`).toBe(expected);
});
