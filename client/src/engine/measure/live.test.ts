import { expect, test } from "bun:test";
import { heapStats } from "bun:jsc";
import { json } from "../../protocol/pins.testutil";
import { LiveRates, RateEstimator } from "./live";

test("the live rate the terminal client shares", async () => {
  for (const { name, steps } of (await json("liverate.testvectors.json")).cases) {
    const estimator = new RateEstimator();
    for (const [bytes, ms, presented, changed] of steps) {
      expect(estimator.observe(bytes, ms), name).toBe(changed);
      expect(estimator.presented, name).toBe(presented);
    }
  }
});

test("servers present independently, and only an irregular receiver is bridged by every lane's fresh hint", () => {
  const live = new LiveRates();
  live.reset(
    new Map([
      ["a", 0],
      ["b", 0],
    ]),
    0,
  );
  expect(live.rate("down")).toBeNull();
  live.download("a", 500, 500);
  live.download("b", 1_500, 500);
  expect(live.rate("down")).toBeNull();
  live.download("a", 1_500, 1_500);
  live.download("b", 4_500, 1_500);
  expect(live.rate("down")).toBe(4_000);

  const frame = (at: number, bytes: number) => ({ id: "u", bytes, nanos: at * 1e6, receivedAtMs: at });
  for (let at = 0; at <= 700; at += 100) live.receiver("a", frame(at, at * 10), at);
  expect(live.rate("up")).toBeCloseTo(10_000, 6);
  const lanes = () => 2;
  // Regular arrivals need no bridge.
  expect(live.bridgedUpload(750, lanes)).toBeNull();
  live.hint("a", 0, 7_000, 1_000, 1_200);
  expect(live.bridgedUpload(1_250, lanes)).toBeNull();
  live.hint("a", 1, 8_000, 1_000, 1_250);
  // A long pause with fresh hints from both lanes bounds the estimate to +25%.
  expect(live.bridgedUpload(1_300, lanes)).toBeCloseTo(12_500, 6);
  // Stale hints return the display to the receiver's rate.
  expect(live.bridgedUpload(1_600, lanes)).toBeNull();
  live.receiver("a", frame(1_700, 17_000), 1_700);
  live.hint("a", 0, 1_000, 1_000, 1_700);
  live.hint("a", 1, 1_000, 1_000, 1_700);
  expect(live.bridgedUpload(1_750, lanes)).toBeNull();
});

test("a slow receiver counting 64 KiB reads never opens with a peak", () => {
  // 9.3 Mbit/s into a server that counts whole reads, a record every 100 ms and the boundary's checkpoint 2 ms after
  // the first record with a read completing between them: that pair alone once showed 28 times the rate.
  const rate = 1_162_500;
  const counted = (ms: number) => Math.floor((rate * (ms + 55)) / 1000 / 65_536) * 65_536;
  const live = new LiveRates();
  live.reset(new Map([["s", 0]]), 0);
  const shown: number[] = [];
  for (const at of [0, 2, ...Array.from({ length: 60 }, (_, i) => 100 * (i + 1))]) {
    live.receiver("s", { id: "u", bytes: counted(at), nanos: at * 1e6, receivedAtMs: at }, at);
    const value = live.rate("up");
    if (value !== null) shown.push(value);
  }
  expect(shown.length).toBeGreaterThan(50);
  expect(Math.max(...shown) / rate).toBeLessThan(1.15);
  expect(shown.at(-1)! / rate).toBeCloseTo(1, 1);
});

test("an hour of 10 ms reports keeps a bounded window and the same steady rate", () => {
  Bun.gc(true);
  const before = heapStats().heapSize;
  const estimator = new RateEstimator();
  // 1,000 B/s in 10 ms reports for an hour.
  for (let i = 0; i < 360_000; i++) estimator.observe(10, 10);
  Bun.gc(true);
  expect(heapStats().heapSize - before).toBeLessThan(4 * 1024 * 1024);
  expect(Math.abs(estimator.presented - 1_000)).toBeLessThan(1e-6);
});
