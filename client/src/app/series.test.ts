import { expect, test } from "bun:test";
import { SERIES_LIMIT, type LatencyBucket } from "../engine/measure/buckets";
import { appendThroughput, idleBucket, upsertLatency, type ThroughputSample } from "./series";

const bucket = (startT: number, endT: number, rtt: number, continuityId = 0): LatencyBucket => ({
  ...idleBucket(startT, rtt, false),
  phase: "latency",
  endT,
  continuityId,
});

test("a revision replaces its bucket and an earlier start slots into order, also after compaction", () => {
  const history = [idleBucket(0, 10, false)];
  for (const [t, rtt] of [
    [10, 12],
    [10, 14],
    [5, 8],
  ] as const)
    upsertLatency(history, idleBucket(t, rtt, false));
  expect(history.map((each) => each.medianRttMs)).toEqual([10, 8, 14]);

  const compacted: LatencyBucket[] = [];
  for (const each of [
    bucket(0, 100, 10),
    bucket(50, 60, 50, 1),
    bucket(100, 200, 10),
    bucket(200, 300, 10),
  ])
    upsertLatency(compacted, each, 3);
  upsertLatency(compacted, bucket(50, 60, 70, 1), 3);
  expect(compacted.map((each) => [each.startT, each.medianRttMs])).toEqual([
    [0, 10],
    [50, 70],
    [200, 10],
  ]);
});

test("compaction keeps the success-weighted median and the worst tail", () => {
  const history = [10, 90, 20, 40, 50].map((rtt, t) => idleBucket(t, rtt, false));
  history.push(idleBucket(4.5, 0, true));
  upsertLatency(history, idleBucket(5, 60, false), 2);
  expect(history).toHaveLength(2);
  expect(history[0]).toMatchObject({
    medianRttMs: 40,
    maxRttMs: 90,
    pingCount: 6,
    timeoutCount: 1,
  });
});

test("throughput compaction keeps the peak, the dip and both ends", () => {
  const rate = (t: number, bytesPerSec: number): ThroughputSample => ({
    t,
    bytesPerSec,
    bytesCumulative: 0,
    dir: "down",
    stage: "download",
    continuityId: 0,
  });
  const history: ThroughputSample[] = [];
  const last = 2 * SERIES_LIMIT;
  for (let i = 0; i <= last; i++)
    appendThroughput(history, rate(i * 10, i === 400 ? 900 : i === 600 ? 1 : 100), 0);
  expect(history.length).toBeLessThanOrEqual(SERIES_LIMIT);
  expect(history.map(({ bytesPerSec }) => bytesPerSec)).toEqual(expect.arrayContaining([900, 1]));
  expect([history[0]!.t, history.at(-1)!.t]).toEqual([0, last * 10]);
});

test("a late revision after compaction replaces its bucket instead of adding to a merge", () => {
  const timed = (i: number, pings = 2): LatencyBucket => ({
    phase: "download",
    continuityId: 1,
    startT: i * 200,
    endT: (i + 1) * 200,
    medianRttMs: 10,
    minRttMs: 10,
    p95RttMs: 10,
    maxRttMs: 10,
    pingCount: pings,
    timeoutCount: 0,
  });
  const history: LatencyBucket[] = [];
  for (let i = 0; i <= SERIES_LIMIT; i++) upsertLatency(history, timed(i));
  upsertLatency(history, timed(SERIES_LIMIT - 3, 3));
  expect(history.length).toBeLessThanOrEqual(SERIES_LIMIT);
  expect(history.reduce((sum, each) => sum + each.pingCount, 0)).toBe(2 * (SERIES_LIMIT + 1) + 1);
});
