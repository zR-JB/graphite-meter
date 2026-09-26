import { expect, test } from "bun:test";
import { connectionQuality } from "./connectionHealth";

function buckets(values: (number | null)[], cadence = 1000) {
  return values.map((rtt, i) => ({
    startT: i * cadence,
    endT: (i + 1) * cadence,
    medianRttMs: rtt,
    pingCount: 1,
    timeoutCount: rtt === null ? 1 : 0,
  }));
}
test("missing evidence and an isolated timeout or RTT spike do not imply unstable connectivity", () => {
  expect(connectionQuality([])).toBe("connected");
  for (const values of [[null], [10, null], [10, 10, 300, 10, 10]])
    expect(connectionQuality(buckets(values))).toBe("connected");
});
test("repeated loss and sustained variation produce distinct warnings", () => {
  expect(connectionQuality(buckets([10, null, null]))).toBe("unstable");
  expect(connectionQuality(buckets([10, 90, 10, 90, 10]))).toBe("degraded");
  expect(connectionQuality(buckets([100, 101, 103, 101, 100]))).toBe(
    "connected",
  );
});
test("a clean tail recovers promptly at idle and measurement cadences", () => {
  expect(connectionQuality(buckets([null, null, 10, 10]))).toBe("connected");
  expect(
    connectionQuality(buckets([null, null, ...Array(8).fill(10)], 100)),
  ).toBe("connected");
  expect(connectionQuality(buckets([null, null, 10, 10], 100))).toBe(
    "unstable",
  );
});
test("persistent low-rate loss uses the observed counts rather than bucket averages", () => {
  const evidence = buckets([10, 10, 10]);
  evidence.forEach((bucket) => {
    bucket.pingCount = 20;
    bucket.timeoutCount = 1;
  });
  expect(connectionQuality(evidence)).toBe("degraded");
});
